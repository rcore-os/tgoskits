# 龙芯 3A6000 的 Axvisor Linux 实板验证

## 1. 启动目标

本设计定义 Loongson-3A6000-LL 上的最小 Linux 客户机验收。板卡服务以
`LoongSon-PC` 提供设备，原生 UEFI axloader 负责装载 Axvisor；Axvisor
再通过现有 LVZ 后端运行 Linux。固件上传完成、宿主启动和客户机用户态执行是
三个独立阶段，只有最后阶段完成交互及功能断言才算通过。

### 1.1 既有能力

现有 ArceOS 实板用例只证明 axloader 到 Hello 的交接。Axvisor 的
`LoongArch64Arch`、`ax_cpu::virtualization::{PerCpu, Vcpu}` 和 Linux
direct loader 已有 QEMU 消费者，本次首先复用这些实现。QEMU 用例的外层 UEFI
与 Linux 客户机的 direct boot 属于不同启动边界。

相关实现入口是
[`arch/loongarch64`](../virtualization/axvm/src/arch/loongarch64/mod.rs)、
[`boot/linux.rs`](../virtualization/axvm/src/arch/loongarch64/boot/linux.rs) 和
[`virtualization`](../components/axcpu/src/arch/loongarch64/virtualization/mod.rs)。
这些代码的存在不表示已经通过本机 LVZ 或客户机验证。

### 1.2 首轮范围

首轮运行一个 Linux 7.1 客户机和一个 vCPU。客户机使用独立分配的 RAM、
虚拟串口、BusyBox initramfs，以及独立文件支持的 VirtIO 块设备夹具。
宿主 initramfs 携带资源，不依赖原系统分区和物理存储直通；宿主控制台始终
由宿主所有，客户机输出经已有控制台复用器转发。

客户机平台继续由现有 LoongArch 虚拟设备图生成。真实宿主的固件资源与客户机
虚拟平台资源分别由各自所有者解释，不要求两者地址布局相同。客户机 SMP、
多客户机、物理设备直通和性能目标需要独立证据，不属于本次验收。

## 2. 资源与装配

板卡配置、客户机配置和构建资源分别保存各自事实，避免以一次成功运行的缓存
替代可复现准备。新增用例沿用 `guest-builds.toml`、`vm_configs` 和
`shell_check_steps` 的现有机制，不建立第二套板卡运行器。

### 2.1 宿主与客户机

板卡运行配置提供完整宿主命令行，包括实际 earlycon 和
`axvisor.vm_start=1`。`vm_configs` 仅将配置加入 `/guest/builtin` 索引，
不隐含创建或启动客户机。Linux 的 `console`、`rdinit` 等参数仅属于
客户机 `[kernel].cmdline`。

Linux direct loader 校验 ELF 的体系结构、段、入口和区间，构造客户机 EFI
system table、memory map 与 FDT，再设置 Linux LoongArch 启动参数
`a0=1`、`a1=cmdline GPA`、`a2=EFI system-table GPA`。这条路径不执行
客户机 UEFI 固件；不得改用不相容的 UHI 参数，或通过跳过地址检查修补启动。

### 2.2 构建输入

客户机内核来自固定 Linux 7.1 提交
`8cd9520d35a6c38db6567e97dd93b1f11f185dc6`。本地源码位置由明确环境参数
选择，编译使用独立输出目录，不能修改用户源码和既有构建。准备步骤记录源码
提交、有效配置、编译器及产物 SHA-256；输入变化后不得继续使用旧产物。

BusyBox 与动态加载器复用现有镜像准备能力。块设备夹具由测试独立生成，
仅包含已知内容，不使用板卡原系统磁盘。`guest_build::prepare()` 中的
`host_prepare` 必须成功后，才允许构建并打包宿主。缺失源码、编译工具或镜像时
应报告真实失败，不静默改用 Linux 6.16 或其他未标明来源的内核。

内核与 initrd 由既有 builtin bundle 按内容摘要打包；设备的文件后端路径不属于
启动镜像字段，不由该 bundle 展开或重写。块夹具通过现有
`image pack-initramfs` 入口放入基底宿主归档的 `/guest/loongson-block.img`，
板卡 `initramfs` 选择该归档，随后附加 builtin 包。客户机磁盘配置使用这一
板端绝对路径，不将构建机路径泄漏给宿主运行时。

## 3. 失败归属

首次上板须保留完整串口日志、宿主及客户机产物身份，按最后可靠状态判断失败
归属。Linux 7.1 的 LoongArch 启动、KVM、页表和中断代码是语义参考，
其实现选择仍须结合 TGOSKits 的所有权边界解释。

### 3.1 虚拟化边界

CPU 层拥有 LVZ 机器状态及进出客户机时的宿主恢复；AxVM 拥有客户机启动、
设备、中断投递和运行生命周期；someboot 与 somehal 拥有宿主固件发现和平台
装配。客户机退出返回 Rust 前必须恢复宿主 FP、TLS 和 CPU-local 状态。
不能通过永久屏蔽宿主定时器、固定忙轮询或跳过二阶段隔离来获得启动日志。

硬件不支持 LVZ 或无法满足二阶段页表契约时必须明确拒绝。故障后的诊断记录
第一处可靠错误与必要寄存器，不把随后的控制台或 panic 故障自动视为根因。
具体底层修复若改变共享契约，应先同步对应设计再实施。

### 3.2 板卡恢复

板卡会话独占串口及启动事务，完成或失败后确认进程退出并释放租约。
重新验证使用板级重启和新的运行日志，不复用仍在运行的客户机状态。
本次不写持久固件、ESP、OTA 状态或原系统分区；若发现必须修改这些资源，
先保留现场并说明依赖和恢复方案。

### 3.3 串口发现与选择

早期日志输出和可休眠的运行时串口输入是不同能力。实板 Hello 已以
`earlycon=ns16550,mmio,0x1fe001e0` 输出成功，但没有 SPCR 默认控制台声明；
仅关闭 AML、保留 earlycon 不能提供管理控制台所需的设备和中断。
采集的 DSDT 则包含 `\_SB.COMA`：`PNP0501`、MMIO
`[0x1fe001e0, 0x1fe001e8)`、GSI 26、电平高有效、共享线路。
因此本机需要恢复命名空间设备发现，并显式选择串口，而不是新增 SPCR 根回调。

运行时选择由 `somehal::boot_console` 统一负责。保留既有 `console=ttyS<N>`、
FDT 别名与 SPCR 的语义，并增加 Linux 7.1 的地址选择语法子集
`console=uart[8250],io|mmio,0x<addr>[,<options>]`。本机宿主使用
`console=uart8250,mmio,0x1fe001e0`，客户机的 `console=ttyS0,115200`
保持独立。地址选择通过
`rdrive::acpi_resource_address_to_device_id(AcpiResourceAddress)` 精确查找
已注册设备，不根据探测顺序猜 `ttyS0`，不建立第二份设备登记表。

显式地址参数只选择已有设备，不创建 MMIO 映射、不执行 I/O，也不重配固件留下的
线路、波特率或 FIFO；可选线路参数不改变现有接管语义。仅支持 `io` 和字节访问
`mmio`，不把未实现的 `mmio32` 等模式当成有效配置。已识别的地址选择若格式错误、
地址为零、溢出、模式不支持或没有匹配设备，返回 `DeviceNotFound`，不得退回
SPCR、其他串口或通用 `ttyS0`。多个 `console=` 继续由最后一个硬件选择决定，
后续虚拟终端不覆盖硬件选择。

UART 的寄存器布局与 GSI 继续来自 `AcpiInfo`，经 NS16550 探测注册和平台
中断控制器解析成 `BindingIrq`，不能把 GSI 26 当作处理器中断线。沿原有
`begin -> adopt -> commit` 完成唯一寄存器所有权转移；没有可用资源或中断时明确
拒绝交互，不用轮询补输入。若其他固件连设备资源也未描述，需要另行提供完整且
经核实的资源配置；本机已有 DSDT 资源，不为尚未出现的缺失增加硬编码旁路。

### 3.4 AML 解释器修复

当前 crates.io `acpi 6.1.1` 在模块级执行合法的
`DerefOf(Index(Package))` 时错误拒绝对子 Package 的引用，实板首先报告
`ObjectNotOfExpectedType { expected: Reference, got: Package }`。
官方 `rust-osdev/acpi` 已通过 PR #286 修复，精确提交为
`36f349539e15a9aeaf57f0c4428f75b046491572`。项目采用官方修复，不复制解释器、
修改 registry 缓存、吞掉 AML 错误或宽泛升级到 7.0 预发布版。

在包含修复的稳定版本发布前，由根 `workspace.dependencies` 统一固定官方 Git
来源和完整 revision，`rdrive` 与 `someboot` 使用同一依赖身份，Cargo 生成锁文件。
这是一项明确的未发布上游修复接入，不是任意 6.1.1 的兼容承诺；该提交还改变了
部分整数转换与 BufferField 接口，须核对两消费者的实际 API、默认功能、许可证与
目标编译。未来回到发布版本时应以包含该修复及通过同样验证为条件，而非只比较包
版本号。回滚同时恢复依赖、锁文件及板卡配置，不涉及持久固件或磁盘迁移。

固定版本的 `Interpreter::new_from_platform()` 会在表加载错误后记录日志并继续，
不能直接作为本项目的成功判定。`rdrive` 改用公开的 `Interpreter::new()` 和
`load_table()` 装配：复用 `AcpiPlatform` 的固定寄存器与 FACS，先装载 DSDT，
再按固件顺序装载 SSDT，每张表检查头部长度及地址范围后映射。任一表加载失败即
传播原始 `AcpiError::Aml`，丢弃尚未发布的解释器，不初始化或发布部分命名空间。
这只是严格的平台装配，不复制 AML 执行实现或修改上游缓存。

表描述符继承平台对固件 backing 的信任，不等于已验证整表校验和；逐表映射覆盖
同步 `load_table()` 调用即可，方法代码由解释器复制拥有。FACS 的可写 backing
则保持整个解释器寿命，并继续由平台内存所有者预留。失败时丢弃解释器不能撤销
模块级 AML 已执行的硬件写入，因此不自动重试，后续诊断仍使用新的板级启动。

`AcpiHandler` 显式保留原发布版遇到 AML `Fatal` 即终止的行为，不继承固定版本
仅记录日志的默认处理。该约束不把原有可选设备求值或 PCI 命名空间诊断改成新的
全局失败策略；其余初始化顺序和资源所有权保持不变。合成的合法及非法 AML
须证明严格装配能接受正常表、拒绝坏表并保留错误，真实 DSDT 及板测继续证明
平台与硬件访问边界。

验收保留同一合法最小 AML 在旧发布版失败、官方修复版成功的确定性证据，并用
真实 DSDT 验证加载；离线环境不能伪造硬件读取来证明命名空间初始化。随后必须在
实板证明 AML 初始化、UART 注册、运行时移交与命令输入。只读资源准备、Hello 和
既有 LVZ 证据各自有效，但不能替代新依赖下的消费者检查和最终 Linux 板测。

## 4. 验收证据

验收从板卡服务的公开入口贯穿到 Linux 用户态，成功断言不能在启动参数回显中
提前命中。运行步骤先等待宿主管理控制台，再执行 `vm console 1` 接入客户机；
客户机功能的判定必须来自随后实际执行的命令。

### 4.1 功能判定

客户机须报告本次 Linux 7.1 和 LoongArch 架构，完成确定性计算、内存文件
写读校验、定时等待后的继续执行，以及独立 VirtIO 块设备的枚举、挂载与内容
校验。块设备的读写权限按配置验证，断言失败须传播到最外层任务工具退出状态。

同一最终版本还须完成第二次独立板级启动，以确认不依赖上一轮残留状态。
两轮均保留源码提交、镜像摘要、实际命令、成功标记和完整日志位置。
重复运行不能替代对已观察到故障的根因分析。

### 4.2 检查范围

只新增板卡配置时，发现入口和真实板测提供装配证明。修改 Rust 时运行
`cargo fmt` 与受影响软件包的 `cargo xtask clippy --package`；修改通用
LVZ 路径时增加相应 QEMU 或 CPU 集成验证。错误修复优先复用已有通用测试，
须先证明旧实现不能满足同一判定。

交付记录分别说明实板通过、定向检查和未覆盖范围。QEMU 或历史 ArceOS Hello
只能作为各自边界的证据；板卡不可用、硬件不支持或必要检查受阻时，开发 PR
保留为草稿并明确当前阻塞，不宣称实机验收完成。
