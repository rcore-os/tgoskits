# AxVM LoongArch 客户机 IOCSR 隔离

## 1. 安全边界

IOCSR 是 LoongArch 处理器访问中断控制器、处理器间通知和平台信息的寄存器
地址空间，不是客户机 RAM。客户机可以自行构造地址，因此“模型没有实现”不能
解释为“允许访问宿主”。本设计约束 AxVM 的软件解释路径，不改变
`ax_cpu::registers` 为合法宿主驱动提供的底层指令。

### 1.1 已核实的问题

`policy/iocsr.rs::resolve_iocsr_read()` 对未识别地址读取宿主 IOCSR，
`policy/vcpu.rs::capture_pinned_host()` 对未认领写执行真实宿主指令。
固定 Linux 7.1 的 EIOINTC 初始化会对 `MISC_FUNC` 地址 `0x420` 做 64 位
读改写，设置 bit 48；当前 AxVM 模型没有认领该地址。这条代码链已经足以证明
隔离缺口，但不证明某次停顿前的客户机确实执行过它。

Loongson 实板任务不授权物理设备直通。仅关闭某个 Linux 驱动、根据地址返回
猜测值或继续使用硬件后备，都不能保证任意客户机指令不改变宿主中断状态。
因此在恢复 Linux 上板之前，先删除 AxVM 的八个 `host_iocsr_read/write_*`
包装器及其全部后备调用；`host_cpucfg()` 和 CPU 层宿主原语保留。

### 1.2 方案与范围

扩展既有 `LoongArchIocsrState`，使支持的访问只操作 VM 自有状态，不建立
第二个 IOCSR owner，不把 CPU 层变成设备模型，也不为本次修复重建设备图。
PCH-PIC 仍通过已有运行代次绑定端口向该状态发布中断。

新建通用 IOCSR 总线会增加登记、生命周期和迁移成本，本次没有相应消费者；
逐地址转发宿主则违反隔离目标。选择在现有模型内实现明确的寄存器子集并严格
拒绝其余访问。目标仍是单 vCPU、单 EIO 节点，不把现有多槽存储视为客户机
SMP、任意路由或物理直通已经受支持的承诺。

## 2. 寄存器契约

寄存器身份与能力由模型固定提供，不能从宿主 IOCSR 复制。以下范围由
`policy/iocsr.rs` 统一解释，宽度是字节数；所有访问必须自然对齐且完整落在
指定寄存器内，不支持的宽度不是自动拆分请求。

### 2.1 平台身份

`FEATURES` 只宣告现有 IPI 与传统 EIOINTC 模型，不宣告调频、温度、节点计数、
MSI、AVEC、重定向或编码扩展。`IOCSRF_VM` 在 Linux 中还会选择四条父线布局，
而当前客户机固件描述单一父线，因此该位保持零；这不是允许客户机访问宿主
硬件的声明，也不改变 CPUCFG 的指令集能力策略。

| 地址 | 访问 | 客户机可见值或行为 |
| --- | --- | --- |
| `0x8` FEATURES | 只读，4 | `EXTIOI \| CSRIPI = 0x18` |
| `0x10` VENDOR | 只读，8 | 小端字节 `Axvisor\0` |
| `0x20` CPUNAME | 只读，8 | 小端字节 `LA64VM\0\0` |
| `0x420` MISC_FUNC | 读写，8 | 仅 bit 48，见 2.2 |
| `0x40000000` EXTIOI_VIRT_FEATURES | 只读，4 | `VIRT_EXTENSION \| ENABLE_OPTION = 0x3` |
| `0x40000004` EXTIOI_VIRT_CONFIG | 读写，4 | 仅 ENABLE bit 1 |

未列出的身份寄存器，包括 `NODECNT 0x408`，明确拒绝。CPUCFG 与 IOCSR 的
`0x40000000` 属于不同命名空间；当前 `guest_cpucfg_value()` 不发布 KVM
签名，本次不增加 KVM 半虚拟化发现。先前返回 `0xf` 却未完整实现的 INT/CPU
编码能力停止宣告，相关 CONFIG 位和 MISC bit 49 写入明确拒绝。

### 2.2 总使能状态

`LoongArchIocsrState` 保存唯一的 VM 级 ENABLE 原子状态，复位为关闭。
`MISC_FUNC` 和 CONFIG 只是它的两个视图，不能各保存一份值，也不能放在
每 vCPU 初始化中反复复位。`CurrentArch::init_vm()` 每次准备新运行都会创建
新的 IOCSR 状态；同 VM 的所有 vCPU 引用它，不同 VM 不共享。

MISC 写仅接受零或 bit 48：写一设置 ENABLE，写零不清除，重复设置幂等。
CONFIG 写仅接受零或 bit 1：零关闭，bit 1 打开；没有可在已使能状态修改的
其他配置位。二者使用同一个原子提交点，拒绝包含其他位的写，而非先裁剪再成功。
这保留传统 MISC 置位合并方式，并提供明确的 CONFIG 关闭入口。

关闭只抑制 EIO 向客户机 CPU 的投递，不丢弃 ISR pending；再次打开后，
已保存且来源使能、路由有效的 pending 必须重新变为可投递。所有 EIO 注入和
待处理中断查询都应用同一门控，不能在 `inject_guest_eiointc_vector()` 的
后备分支绕过。IPI 与客户机定时器不受 EIO 总开关控制。

总门控是本虚拟模型明确承诺的语义，不把 QEMU 或 KVM 中未核实的硬件门控行为
当成证明。模型状态以 Release 发布、Acquire 观察；软件待处理位仍经既有
运行代次绑定路径通知，不能清除或重放宿主 ESTAT。

### 2.3 中断与邮箱

IPI、mailbox 和 ISR 保留现有逐 vCPU 槽；EIO 配置银行沿用当前局部视图，
本次不扩展节点间路由。存储型寄存器、一次写置位和一次写清除必须分别解释，
不能因地址落入 `0x1400..0x2000` 就成功吞掉未知访问。

| 地址或半开区间 | 宽度 | 权限与效果 |
| --- | --- | --- |
| `0x1000` IPI_STATUS | 4 | 只读 pending |
| `0x1004` IPI_EN | 4 | 读写来源使能；IPI 电平由 `STATUS & EN` 派生 |
| `0x1008` IPI_SET、`0x100c` IPI_CLEAR | 4 | 只写，分别 W1S、W1C |
| `[0x1020,0x1040)` 四个 mailbox | 4、8 | 读写；4 字节访问保留同邮箱另一半 |
| `0x1040` IPI_SEND | 4 | 只写，验证完整目标与 action 后发布 |
| `0x1048` MBUF_SEND | 8 | 只写，更新有效目标邮箱的指定半字，不自行制造 IPI |
| `0x1158` ANY_SEND | 8 | 只写，见下一段 |
| `[0x14a0,0x14c0)` NODEMAP、`[0x14c0,0x14c8)` IPMAP | 4 | 读写既有 EIO 配置银行 |
| `[0x1600,0x1620)` ENABLE、`[0x1680,0x16a0)` BOUNCE | 4 | 读写既有 EIO 配置银行 |
| `[0x1700,0x1720)` 既有 ISR 别名、`[0x1800,0x1820)` ISR | 4、8 | 读 pending、写 W1C |
| `[0x1c00,0x1d00)` ROUTE | 4 | 读写传统位图路由，不支持编码模式 |

ANY_SEND 只访问有效目标的普通 32 位读写银行：IPI_EN、邮箱半字或上述 EIO
配置银行；不允许递归发送、只写命令、ISR W1C 或身份寄存器作为嵌套目标。
先验证目标、完整范围、读写权限和字节保留掩码，再原子合并未保留的字节。
不存在目标、访问失败或不支持的嵌套目标不能被零值读取或忽略写结果掩盖。

发送命令的合法 `BLOCKING` bit 31 必须接受，不能在保留位检查中误拒绝固定
Linux 的请求；它不授权等待宿主硬件完成。目标有效性按本 VM 已配置的 vCPU
身份判断，不把按最大 ID 分配的数组空洞当作存在的目标。

邮箱半字合并及 ANY_SEND 的保留字节更新使用对应槽的原子读改写，避免覆盖并发
更新。来源 pending 是持久事实，通知只是推进提示；先发布状态，再在不持有
`run` 锁时调用现有端口。旧运行通知失败不回滚已提交的寄存器写，也不得改发到
新运行。无效请求必须在提交前失败，不发布任何通知。

## 3. 解释与失败

原始机器退出仍由 CPU 层提供；寄存器模拟在 backend 卸载、宿主上下文恢复后的
`finish_exit()` 阶段完成。只有 CPUCFG 继续需要
`LoongArchPinnedHost::Cpucfg`，IOCSR 的读写不再需要固定 CPU 快照或副作用。

### 3.1 原子拒绝

解码器校验完整 22 位 IOCSR opcode `0x19200..0x19207`，由它获得方向、
宽度、寄存器号和地址。只匹配高 10 位不足以授权访问；不支持的指令必须形成
可报告的拒绝，而非宿主 panic。指令源、宽度、自然对齐、完整区间、权限、
保留位和发送目标全部有效后，才允许修改模型、目标 GPR、客户机 PC 或通知。

成功读按架构规则进行符号扩展，写截取指令宽度，零寄存器保持零；只有完整
成功的访问推进 PC。失败不修改模型、GPR、PC 或通知状态。限制 32 位银行
只接受 4 字节访问，避免先写第一槽再发现第二槽越界；不能用部分提交后回滚
来代替预检。

### 3.2 故障闭环

IOCSR 拒绝由拥有值记录携带 PC、原始指令、地址、方向、宽度和类型化原因，
经 `LoongArchVmExit`、`LoongArchExitKind` 传到 `handle_exit()`，再产生
现有 `AxVmError::unsupported`。非法 GSPR 指令至少保留 PC 和原始指令。
不直接从 `finish_exit()` 返回需要最终保留详情的解释错误，因为
`engine.rs` 会把非 OOM 错误折为通用 `InvalidState`。

最终仍沿 `run_owner()` 的 `VcpuExitOutcome::Fault` 交给控制 owner 停止
客户机。该记录不是第二个错误 owner，不改变公共 engine 错误接口，不增加
重试、硬件后备或继续执行。错误消息中的地址只用于诊断，不能用格式字符串
重新解析并控制行为。

## 4. 迁移与验证

本变更收紧此前不安全或未完整实现的行为，不修改配置格式、持久磁盘或固件。
所有后备调用及文档在同一序列迁移，不能保留可选择的旧直通模式。
源码回退可以恢复旧版本，但隔离修复缺失时不得恢复不可信 Linux 上板验收。

### 4.1 设计核对

实现前由独立领域核对者检查本设计与固定 Linux、现有退出闭环、固件拓扑及
状态生命周期一致。Linux 参考固定为
`8cd9520d35a6c38db6567e97dd93b1f11f185dc6` 的 `cpu-probe.c`、
`irq-loongson-eiointc.c`、`smp.c` 和 `asm/inst.h`。本地 QEMU 11.1.1
只作为实现参考，未核实其与官方发布源码逐字一致，不提升为 ISA 规范。

当前 Linux 构建关闭 SMP 与 PARAVIRT，启用 MACH_LOONGSON64、ACPI 和
LOONGSON_EIOINTC。这解释最小消费者，不是只检查该镜像就能允许任意其他
IOCSR 访问的理由。任何测试通过都不能替代设计核对或最终 Linux 实板验收。

### 4.2 最小充分证据

现有测试没有覆盖完整 IOCSR 指令解释与隔离，因此在生产模块内增加少量行为
测试：从真实解释入口建立 pending，再通过 MISC/CONFIG 验证共享、门控与重启；
通过非法访问验证拒绝无副作用；通过现有退出处理层验证最终 Fault 原因保留。
同 VM 上下文共享开关与不同 VM 隔离合入这些行为，不另写常量回读测试。

测试不执行宿主特权 IOCSR 指令，不复制生产模型，不用 `#[path]` 引入源码。
LoongArch cross-test 已在启动、逐处理器和异常表符号的链接阶段失败，未执行
测试，因此改用 4.3 的真实内核入口，不以零测试或 x86 host 图冒充覆盖。
恢复错误实现或删除关键验证时，同一判定必须失败，并保存确定性红绿证据。

Rust 改动运行格式化、受影响包 Clippy 和提交后的增量标准库测试；后者只覆盖
实际选中目标。CPU guest-entry 实板用例独立证明 LVZ 与宿主 timer，不能替代
这里的软件隔离证明。最终还需真实 Linux 启动、交互、RAM、定时等待与只读块
夹具，并在同一最终版本完成第二次独立板级启动。

### 4.3 私有行为测试的内核装配

行为测试仍由 AxVM 的 `policy/iocsr.rs` 和 `policy/vcpu.rs` 所有，不搬入集成
测试文件。两个模块在 `cfg(any(test, axtest))` 下编译；同一测试函数体在
`cfg(axtest)` 下由 `#[axtest::axtest]` 注册，普通单测路径使用 `#[test]`。
AxVM 的既有 `axtest` feature 增加可选普通依赖 `axtest`，使作为普通依赖编译
的库也能解析注册宏；未启用该 feature 的产品构建不引入测试运行器。

独立的非发布测试包 `virtualization/test_crates/axvm-kernel-tests` 只负责真实
内核的链接与执行。它通过普通 Cargo 依赖使用 AxVM，不包含生产源码，不公开
私有测试入口，不添加启动符号桩。其 `harness=false` 的 `loongarch_iocsr`
目标链接 `ax-runtime`、`ax-std`、AxVM，由一个 `#[axtest::tests]` 内联模块
提供唯一 `main` 和运行器；该模块不增加替代上述行为的空测试。库中的单函数
注册宏不能生成第二个 `main`。

测试包的 `axtest` 元数据独立选择 `runtime = "axvisor"` 和 LoongArch 裸机
目标，required features 为 `axtest,tls`；包自身的同名 feature 分别显式转发
`axvm/axtest` 与 `axvm/tls`，不能假设 ktest 会启用依赖的同名 feature。
不扩展 AxVM 原包的 AArch64 测试
矩阵：当前 ktest 对包级 `target_triples` 与各 test target 做笛卡尔展开，
直接向原包加入 LoongArch 会把既有 AArch64 客户机测例错误地交给 LoongArch，
反向也会把仅有 LoongArch 私有测试的空运行交给 AArch64。复用独立测试包
边界比修改整个 ktest 选择协议或包装既有客户机负载更小。

该套件使用单核、无客户机配置和无启动选择器的 Axvisor 构建，QEMU 只接必要
固件与串口，不连接宿主磁盘、网络或 Linux 资源。包内
`host-initramfs.toml` 显式设置 `vm_configs = []`，使既有装配入口生成空的
客户机目录归档，并明确选择内存根，而不隐式准备宿主磁盘。
`cargo xtask ktest qemu`
负责真实初始化及失败传播；LVZ QEMU 版本必须由实际启动命令确认。此入口
证明真实 LoongArch 生产解释器、状态模型和 Fault 闭环，不声称已经运行
客户机或验证宿主物理 IOCSR、中断控制器及 LVZ 页表硬件。

通过具备 LVZ 能力的 QEMU 执行下列唯一测试单元；包内构建与运行配置由 ktest
发现，不依赖交互构建快照或本地 Linux 客户机资源。

```bash
cargo xtask ktest qemu --package axvm-kernel-tests --test loongarch_iocsr --arch loongarch64
```

链接器 `KEEP(.axtest_array)` 只保护已进入链接的描述符，不能据此假定 rlib
归档成员必然被抽取。验收必须在同一运行中看到
`iocsr_pending_gate_and_vm_lifetime`、
`iocsr_rejection_preserves_all_effects` 和
`iocsr_backend_gate_and_fault_exit` 三项实际执行、全部通过及套件成功；
只满足运行器的非零测试门禁仍不充分。缺少描述符时保留产物并修正内部链接
装配，不能增加公共测试 API、复制测试或放宽门禁。红绿验证使用同一套件与
断言，仅临时恢复明确的错误行为，完成后精确还原生产版本。
