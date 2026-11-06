# axloader RISC-V UEFI 启动设计

## 1. 目标与固件边界

本设计对应 [#2691](https://github.com/rcore-os/tgoskits/issues/2691)。目标是在 riscv64 UEFI 环境复用设备发现、HTTP 启动事务、串口身份匹配和镜像校验，使独立目标内核实际运行，并取得启动参数、归档内容及串口交互证据。当前已在 BananaPi 从实体 U 盘直接运行 axloader，经网络交接单核 ArceOS，并通过启动资源、参数、归档和应用输出检查。其他内核、对称多处理与 OTA 尚未完成实际验收。

### 1.1 既有实现

`bootloader/axloader/src/loader` 是依赖固件 TCP4、UDP4 和 IP4Config2 的 x86_64 前端。`BootJobStore`、`ElfImage`、HTTP 请求解析和 `LoaderSerialStatus` 已独立于该传输层。`httpboot-protocol 0.5.0` 已包含 `BootArch::Riscv64`，无需增加另一套协议或改变 v6 消息格式。LoongArch 的 `axstd_loader` 已退出启动服务并拥有完整 OS 运行时，不适合直接作为原生固件前端。

### 1.2 BananaPi 固件

测试通过 ostool 串口和一次性 F2 Boot Manager 进入 EFI Shell 核验固件能力。硬件是 SpacemiT K3 Com260 IFX、8 GiB；当前启动链是 U-Boot SPL、EDK II、GRUB 2.14、Bianbu Minimal UEFI，SPL 构建时间为 2026-10-16。Shell 报告 `UEFI Interactive Shell v2.2`、`UEFI v2.70 (SPACEMIT, 0x00FF00FF)`，Linux 也报告 `efi: EFI v2.7 by SPACEMIT`，并成功继续启动。

固件句柄包含同一物理网卡上的 SNP、UDPv4 Service Binding、IPv4Config2，以及 Serial I/O、RNG 和标准 RISC-V Boot Protocol。没有发现 TCP4 Service Binding，不能据此改用既有 TCP4 前端。Shell 的 `ifconfig -l` 显示介质存在、静态策略、地址 `0.0.0.0`；这是配置现状，不是 DHCP 失败。`dmem` 的摘要显示 DTB 为零，但 2026-11-04 读取原始 GUID 配置表后发现真实 FDT 位于 `0x123fe7000` 的 `BOOT_SERVICES_DATA`，不能将摘要当作缺少 FDT 的证据。该树还含未被 EFI 保留的静态 `no-map`；axloader 必须通过显式提供的匹配 DTB 覆盖默认树，并保持完整校验。

板载 UFS ESP 的 `EFI/BOOT/BOOTRISCV64.EFI` 属于当前系统的 GRUB。测试使用独立 U 盘，不覆盖该文件，也不修改原启动顺序或 EFI 启动变量。

## 2. 实施方案

优先保留原生固件环境，在 RISC-V 前端使用 SNP 提供以太网收发，由 `smoltcp` 完成 DHCP、IPv4、UDP 发现与串行 HTTP 连接处理。HTTP 路由和启动事务仍由现有对象维护，避免为不完整的固件协议增加第二套 HTTP 业务状态。

### 2.1 方案比较

平台差异必须在固件入口和传输边界收敛。下面的选择以 BananaPi 的实际固件能力为依据。

| 方案 | 与现有代码的关系 | 判断 |
| --- | --- | --- |
| 只启用现有 TCP4 前端 | 改动小，但目标固件缺少协议 | 无法满足板上发现和远程启动 |
| 引入 ArceOS 运行时 | 可复用 OS 网络，但还需 K3 OS 驱动与运行时退出契约 | 引入与原生 UEFI 目标无关的依赖和硬件所有权 |
| 原生 UEFI 加 SNP 网络 | 复用 firmware NIC、公共 HTTP 和启动事务 | 作为直接板卡验证方案 |
| 更换板卡固件 | 可提供完整协议，但属于独立板级固件移植 | 不作为本任务默认部署动作 |

新 SNP 后端只用于明确选择的原生 RISC-V 前端；x86_64 继续使用其现有固件网络后端。共享 HTTP 层只依赖可失败的读取和写入能力，不暴露 smoltcp socket 或 TCP4 token。

### 2.2 EFI 产物

固定 `nightly-2026-09-04` 没有内建 RISC-V UEFI 目标。使用独立的仓库目标描述，明确 ELF 对象格式、RV64IMAC、`lp64`、无浮点、PIE 和禁用链接松弛；不能伪造工具链已有目标。`cargo xtask axloader build` 负责该目标的 `core/alloc` 构建、链接和固定工具链 `llvm-objcopy` 转换。

固件镜像在 ELF 开头包含 PE32+ 头，机器号为 `0x5064`，子系统为 EFI Application，文件名为 `EFI/BOOT/BOOTRISCV64.EFI`。裸汇编入口在任何可重定位全局访问前应用 `R_RISCV_RELATIVE`，随后执行 `fence.i` 再进入 Rust。产物校验必须检查段和相对地址、文件与虚拟大小、入口执行权限及动态重定位种类；异常重定位不能静默忽略。

该固定编译器对 `os=uefi` 要求 `is-like-windows`。为避免 COFF bitcode 标记进入 ELF 链接，此目标显式关闭 LTO 与嵌入 bitcode；`riscv64_uefi/linker.py` 仅移除 ELF 不支持的 `--nxcompat`，其余参数交给固定工具链的 `rust-lld`。不要直接恢复工作区 release 的 fat LTO。

### 2.3 控制台身份

`SerialBeacon` 复用标准 Serial I/O，选择与固件控制台路径唯一匹配的 UART，并读取线路参数；缺少协议或选择不明确时不报告 ready。RISC-V 前端不增加文本控制台替代路径或板卡专用声明。

服务器通过真实串口收到的本次 `serial_id` 完成匹配，保留 epoch、撤销和重启后拒绝旧绑定的规则，不能以配置中的 MAC 替代身份帧。

身份帧使用协议本来支持的 LF 分隔。2026-11-04 实板原始 UART 中，旧发送端显式 CRLF 经固件 Serial I/O 路径后变成 CRCRLF，39 条完整帧均被严格解码器拒绝；发送端改为 LF 后，保持服务端解码规则不变，真实 `cargo xtask arceos board` 已绑定 `/dev/ttyUSB0`。固件未转换时的 LF 与本板转换后的 CRLF 都是已有协议格式。

## 3. 所有权与交接

网络、内核镜像和交接载荷各有唯一所有者。公共层负责事务，后端负责机器和固件操作；发布状态不能替代资源停稳。

### 3.1 网络资源

SNP 后端独占所选固件 NIC。接收缓冲区由后端拥有，发送缓冲区必须保存到 SNP 的完成状态确认回收，不能在 `Transmit` 返回时立即释放。队列容量、帧长度、HTTP 请求头和上传长度保持有界。关闭先停止接纳，排空或取消连接，完成 SNP 发送回收并停稳设备，再销毁缓冲区。超时或无法确认设备停稳时禁止进入目标内核。

smoltcp 的 `send_slice` 成功仅表示数据入队。开始交接前必须确认 `202` 响应已经实际发出并完成所要求的 TCP 确认，再停稳 NIC。`GetStatus`、发送回收或 `Shutdown` 失败时，由原所有者继续保有 NIC 和全部在途缓冲区并复位，不能通过普通 `Drop` 释放仍可能被固件或设备引用的内存。

实板连续请求已证明单个 TCP socket 存在监听空窗：HTTP 响应已到达客户端，但原连接仍在 FIN 收尾，新连接会被拒绝。实现使用两个固定容量的 TCP socket，维持一个当前事务和一个待接入连接；HTTP 业务仍串行，启动事务状态仍唯一。`poll_ingress_single()` 在相邻帧之间重建可用监听，保持原 RX 预算；当前事务关闭后先结束本次入站轮询，由外层释放 handle，再处理下一帧。网络轮询只重置不属于当前事务且没有待发 RST 的关闭 socket，避免清空发送队列后误判响应已获确认。当前连接的完成仍检查真实发送确认、有效 DHCP 租约与关闭状态。

双 socket 第一版的实板连续两次查询已返回 `200`，但随后立即 RST 并重连仍失败。逐帧重听和 `Network::promote_tcp()` 修正监听顺序；双槽满载时，`SynBackpressure` 只丢弃没有既有四元组的合法新 HTTP SYN，让客户端重传，保留原连接的 ACK、FIN 和 RST。实板抓包已记录同一个 SYN 重传后收到 SYN-ACK，随后完成 `202` 和真实内核交接；独立取消后重启用例仍需最终验收。串口定时同时修正了把 `CheckEvent` 的 `Ok(false)` 当作到期的问题，同一实板 2 秒采样由 93 帧降为 8 帧，符合 250 ms 周期。

时间来自经验证的固件 FDT `timebase-frequency` 和 RISC-V `time` 计数器；不把轮询次数当作真实时间。DHCP 未取得地址时不发布虚假在线状态。广播和 HTTP 必须使用同一网卡及同一个租约地址。

`BootIdentity::establish()` 从固件 RNG 取得随机种子，通过域分离生成本次固定的 epoch 和 serial ID。没有可靠随机数时停止，不以 RTC、地址或易失计数猜测跨启动唯一性，也不接受宿主注入的身份种子。

### 3.2 内核入口

`TargetSpec` 为 RISC-V 声明 `EM_RISCV = 243` 和 `efi_pe_entry`。公共 `ElfImage` 校验架构、可执行段和入口符号，加载资源由 `Allocation` 保持到交接。复制后刷新指令缓存。入口、目标分配、cmdline、LoadFile2 initramfs 和元数据仍通过 `PreparedHandoff` 发布，不增加第二种传输协议。

someboot 的 RISC-V EFI 入口先建立映像私有栈和 `gp`，完成重定位及一次性 BSS 初始化，再保存 EFI 参数。使用 `RISCV_EFI_BOOT_PROTOCOL` 查询 boot hart ID，并从 EFI 配置表取得 FDT；不能固定 hart 0，不能读 S 模式不可访问的 `mhartid`。随后构造现有 `CpuBootInfoV1`，接入公共启动流程；不得重新进入会清理 EFI 状态的直接启动入口。

接收入口按 [UEFI 2.11 的 RISC-V 契约](https://uefi.org/specs/UEFI/2.11/02_Overview.html#risc-v-platforms) 接受 Bare 或 EFI 内存恒等映射的固件环境，切换栈和 CPU 记录前屏蔽 SIE，预交接错误恢复固件寄存器后返回。固件页表保留至 `ExitBootServices` 和 `SetVirtualAddressMap` 成功；随后 `firmware::retire_boot_page_tables()` 用现有 CPU 接口切回 Bare 并失效本地地址转换，再发布可回收内存图。这个顺序防止早期分配器复用仍被 SATP 引用的固件页表；不能在仍调用固件服务的入口处清 SATP。启动服务调用依赖固件的轮询完成能力，尚未验证依赖 S-mode 定时中断的其他固件；本板先使用单核目标，不把它解释为 EFI 对称多处理启动证明。

当前固件安装的 FDT 位于启动服务数据页且存在未被 EFI 保留的静态 `no-map`；直接路径使用同 ESP 显式提供的匹配 DTB，完成 fixup 并保存在 ACPI reclaim 页后发布。临时 GRUB 验证可以通过 `devicetree` 发布匹配树，直接 U 盘启动则由 `Fdt::prepare()` 装载同 ESP 的 `board.dtb`，完成标准 fixup、配置表安装和所有权维护。原系统 `/boot/spacemit/6.18.3-generic/k3_com260_ifx.dtb` 中有 11 处静态 `no-map` 未被 EFI 保留，其中包含 SRAM、休眠和显示范围；GRUB 启动 axloader 已明确拒绝第一个 `0xc0876000` 范围。不能直接把该 OS DTB 当作合格输入。缺少 FDT 或有效计数率时明确停止，不猜测频率或硬件布局。

本板匹配 DTB 来自现场 SPI FIT 的 `k3_com260_ifx/fdt_2`，通过只读提取和 FIT 校验后部署到测试 U 盘。其全部静态 `no-map` 均被 EFI `RESERVED` 覆盖，并已通过外置介质的标准 fixup 与实际 ArceOS 启动验证；没有写入 SPI 或覆盖原系统设备树。验收产物摘要见 5.4 节。

退出固件服务和内存图处理沿用 someboot 的公共 EFI 路径。保留原有 SBI 直接启动、次核 HSM 和非 EFI 行为。所有网卡、身份帧计时器及 HTTP 对象必须先释放，再进入接收内核；接收内核退出服务后不得调用固件启动服务。

### 3.3 错误与恢复

上传错误、摘要不符、错误架构或入口和载荷准备失败均通过现有错误响应返回，保持未启动状态并释放本次资源。交接前失败允许发起新的事务；退出服务后失败不能恢复旧的固件网络服务。不要用延长超时、默认参数、忽略协议缺失或只打印准备完成标记掩盖失败。

原生 EFI 首次装载和 HTTP 内核交接分别验证。2026-11-04 实体 U 盘完整启动已串联验证两者，旧映像因固件默认树不满足交接契约失败，显式外置树版本通过。A/B OTA 的既有格式不变；RISC-V 尚未完成实际 trial、确认和回滚验证前，文档不得宣称对应升级能力已验收。

### 3.4 U 盘设备树

原生 U 盘在 `EFI/BOOT/BOOTRISCV64.EFI` 放置 loader。`Fdt::prepare()` 优先读取同一 LoadedImage 文件系统中显式提供的 `EFI/BOOT/board.dtb`，完成校验和标准 fixup 后覆盖固件表；该显式输入存在但损坏时失败，不扫描其他磁盘。只有文件确实缺失或映像没有文件系统时才使用既有固件表；完整位于 `EfiACPIReclaimMemory` 的有效固件 FDT 可以直接借用，其他内存类型或损坏输入仍报错。

外部 DTB 由 `Fdt` 唯一持有 `EfiACPIReclaimMemory` 页，文件与扩容上限为 16 MiB。`ValidatedFdt::parse()` 复用 `fdt-raw`，检查完整 token 边界、块范围、对齐、保留记录和非零计数率，限制节点深度为 16；头部可解析不能替代完整边界证明。固件 fixup 返回 `BUFFER_TOO_SMALL` 时只按有效的新长度受限重试，其他失败不安装配置表。

DT Fixup 的 ABI 由 GUID 选择：`60ed6ba9-dfef-4799-ac7b-75e0f833456c` 使用无 flags 的新接口，`e617d64c-fe08-46da-f4dc-bbd5870c7300` 使用旧接口且只请求 `APPLY_FIXUPS`。两者使用独立私有 FFI 类型，不靠相同的 revision 猜测签名。配置表通过标准 `InstallConfigurationTable` 安装；不使用旧实现扩展的 `RESERVE_MEMORY` 或 `INSTALL_TABLE` flags。

`MemoryReservation` 保留每个静态 `reg` 元组的地址、长度和 `no_map`。`ValidatedFdt::validate_uefi_reservations()` 按 [DTSpec 3.5.4](https://devicetree-specification.readthedocs.io/en/latest/chapter3-devicenodes.html#reserved-memory-and-uefi) 检查完整 EFI 覆盖：带 `no-map` 的范围仅接受 `RESERVED`；普通静态范围接受规范要求的 `BOOT_SERVICES_DATA`，并保留对其他明确保留类型的兼容。`CONVENTIONAL`、loader 分配及 Boot Services Code 不能证明静态保留。验证继续使用分配外置 DTB 页之前的同一份内存图，避免本次分配伪造保护证明。普通保留区中的 Boot Services Data 由接收端在分配前按 FDT 再次保留；这与 DTB 自身必须位于 ACPI reclaim 内存是不同的生命周期要求。

只有 `size` 的动态保留节点是交给 OS 的分配请求，不要求固件提前建立静态保留记录。解析器校验其非零 `size`、可选 `alignment` 及 `alloc-ranges` 的单元宽度与溢出，保留节点而不提前分配。存在 `reg` 时以静态范围为准；损坏的 `reg` 不能退回动态请求。此处允许传递合法请求，不表示接收内核已实现 CMA 分配器。

owner 跨网络循环和接收内核调用保持存活。EFI 应用返回时先恢复替换前的配置表；原来没有表时才卸载本次表，再释放页。若表被其他所有者替换，或恢复、卸载失败，继续保留页。退出 Boot Services 后禁止析构调用固件接口。此生命周期沿用 [UEFI 的 DTB 内存类型要求](https://uefi.org/specs/UEFI/2.10/04_EFI_System_Table.html#devicetree-tables) 和 [DT Fixup 的分版协议](https://github.com/U-Boot-EFI/EFI_DT_FIXUP_PROTOCOL)。2026-11-04 已从实体 FAT32 U 盘 `FS3:\EFI\BOOT\BOOTRISCV64.EFI` 直接执行 loader，记录 `loader_fdt_installed: source=EFI/BOOT/board.dtb`；标准 fixup、自动串口绑定、网络 ELF/initramfs 传输及 ArceOS 四个既有成功标记均已通过，此次启动未经过 GRUB 或 Linkr。

旧 loader 在本板固件默认树上返回 `loader_fdt_unavailable: COMPROMISED_DATA`：该树位于 `BOOT_SERVICES_DATA`，且存在未被 EFI 保留的静态 `no-map`。仅复制到长期内存不能修复保留区契约，因此不保留 BS Data 复制分支。显式匹配 DTB 优先于默认树，不增加板卡专用配置；同一固件和 U 盘上，旧映像在 FDT 阶段失败，外置 DTB 方案完成真实网络交接。验证继续要求原始文件边界、fixup 前后完整结构与全部保留元组均合法。

### 3.5 接收内核的映射边界

EFI 内存图负责 RAM 范围，FDT `no-map` 还要求排除普通映射。`fdt::memories()` 在 EFI 启动时消费退出服务后已保存和规范化的 `mem::memory_map()`，按 [DTSpec 3.4.1](https://devicetree-specification.readthedocs.io/en/latest/chapter3-devicenodes.html#memory-node-and-uefi) 忽略 FDT `/memory`；因此该节点缺失或与 EFI 不一致也不会遗漏内核、栈及启动数据。直接启动仍从 FDT 取得 RAM。两条路径共用 `no-map` 排除操作，EFI 路径跳过 MMIO，并合并相邻同权限片段，避免固件按所有权细分内存造成映射清单膨胀。

由 someboot 的 FDT 内存层提供按页扩展的静态 `no-map` 范围及候选 RAM 扣除操作，遍历节点的全部 `reg` 元组。完整解析和物理保留必须发生在 `ram::init()`、`save_fdt()`、逐核区与页表分配之前，不能等建立页表时才发现早期分配已占用这些页。分配器继续保留这些物理范围；RISC-V RAM 身份映射、直接映射和动态平台的普通 `Reserved` RAM 映射只消费扣除后的片段。内核映像、逐核区、启动数据等必须映射的资源若与排除页重叠，启动明确失败，不用额外别名或静默切片绕开。页表使用的映像扩展尾部同样参与检查。

`someboot::mem::NoMapRegions` 是从当前 FDT 派生的短生命周期值，不是额外登记表；FDT 层维护唯一提取算法。其范围按页扩展、排序并合并，提供候选区间扣除和必须映射区间的冲突检查。`reserved_ram_regions()` 在同一所有者内生成最终保留 RAM 映射，由动态平台薄适配消费；没有为宿主测试增加伪内核 ABI。无效范围、溢出或有界容器容量不足必须报错，不允许静默丢弃保留区或可映射片段。恢复早期整段 RAM 映射、保留 RAM 整段映射、仅保留首个元组，分别使生产路径的同源测试失败；修复后 someboot 86 项测试及两包 Clippy 27 项通过；实板已进入 RISC-V 内核并完成早期页表和运行期内存管理初始化。

该方案复用已有 EFI 内存图和 FDT 保留信息，不增加另一份保留区登记表。只在 loader 校验会遗漏接收侧映射，只改早期页表会被运行期重新映射；新增跨架构 `MemoryType` 变体则扩大了与本次问题无关的接口和匹配范围，因此优先让既有映射消费者共享 FDT 排除操作。验证需同时证明 EFI RAM 来源优先、保留区不可分配、普通映射无排除页、无序和重叠范围不会丢失，以及必须映射的资源冲突会失败；算法证明与真实 RISC-V 启动证据分别保留。

### 3.6 运行时串口接管

BananaPi 的固件树将 UART 描述为 `ns16550`，但 K3 需要保持 IER 的 UUE 单元使能位。[OpenSBI K3 平台支持讨论](https://lists.infradead.org/pipermail/opensbi/2026-September/010600.html) 及其后续固件兼容性修正记录了同一硬件要求。原 `Ns16550::mask_all()` 将整个 IER 写零；ArceOS 从固件控制台接管时调用该方法，导致串口输出消失。日志最后半行只是 UART 中待发送字节的位置，不能用它判定调度器停机。

`some-serial::ns16550::mask_interrupt_enables()` 只清除已定义的标准中断使能，保留扩展控制位，并同时供普通与紧急接管使用；紧急路径继续读回 IER，保证屏蔽写入完成后才访问 TX。增强已有的固件 FIFO 接管和紧急接管测试：旧实现均把期望的 `0x40` 清成 `0`，修复后 RISC-V qemu-user 的 64 项串口测试通过，定向 Clippy 两组通过。实板随后输出全部启动标记。本改动不宣称实现 XScale 扩展 IRQ、普通重新配置或 AIA 支持。

## 4. 服务端与兼容

`drivercraft/ostool` 提供设备发现、串口绑定和网络启动事务；接收内核架构必须与服务端生成的 EFI 入口一致，前端也必须保留发现设备的真实架构。

### 4.1 配套修改

服务端 `device::v5_boot_manifest()` 按真实架构选择 EFI 入口：x86_64 使用 `__x86_64_efi_pe_entry`，RISC-V 与 LoongArch64 使用 `efi_pe_entry`，其他未定义 EFI 契约的架构明确拒绝。发现页创建配置时携带架构，`BoardEditor` 校验 MAC 对应的设备架构，防止配置缺失后默认为 x86_64。HTTP 成功状态检查须消费完整响应体，再继续事务，避免过早关闭连接。

既有继电器配置若仍使用已被解析器拒绝的 `usb_id` 选择器，应迁移为 `usb_path` 并保留原设备路径。该兼容迁移不改变板卡 MAC、启动架构或串口身份规则，也不意味着允许歧义匹配。

设备列表和串口绑定状态机复用已有逻辑。HTTPboot 继续校验 loader 上报的稳定 MAC；调试使用隔离服务或会话资源，不改写用户持久板卡配置。

### 4.2 迁移与回滚

既有 x86_64、LoongArch64 请求和 EFI 入口保持兼容；不改变协议版本和持久状态布局。新目标和新传输各有明确选择边界。回滚恢复旧 loader 和对应构建配置即可，不依赖修改板卡 Flash。服务端架构入口修复先验证原有目标，再与新 loader 联调。

## 5. 验证与交付

按照 `test-quality` 复用已有 ELF 校验、启动事务、身份帧和 HTTP 功能测试，新增证明应对应传输及固件接收路径的独立风险。源代码中的架构常量出现不能作为启动证明。

### 5.1 最小验证矩阵

新代码先以失败用例证明目标或后端尚未支持，再验证同一路径通过。实施顺序是产物与 EFI 最小启动、SNP DHCP 和 HTTP、串口匹配、真实内核交接，最后完成完整服务端操作链路。

| 边界 | 必须取得的证据 |
| --- | --- |
| EFI 产物 | 真实固件从内存加载正确的 RISC-V PE，入口及重定位执行成功 |
| 网络 | BananaPi 实际 DHCP、MAC/IP/架构/版本上报及 TCP 请求响应 |
| 事务 | 错误架构、长度和摘要被拒绝，合法上传成功，取消后可再启动 |
| 身份 | 本次身份帧经真实串口收到，绑定生效，重启后旧 epoch 与绑定拒绝 |
| 交接 | 独立目标输出参数，验证 initramfs 文件，完成串口交互 |
| 兼容 | 受影响软件包的定向 Clippy，以及既有前端的相关功能证明 |

执行使用 `cargo xtask` 的构建、静态检查和测试入口。编辑 Rust 后运行 `cargo fmt`，最后核对差异和引用；没有更改 Rust 的设计阶段只执行文档检查。启动顺序、目标描述和推荐调试路径的实施改动须同步更新 `arch-platform-porting` 参考资料。

### 5.2 验收边界

ABI 依据为 [UEFI 2.11 RISC-V 约定](https://uefi.org/specs/UEFI/2.11/02_Overview.html#risc-v-platforms)、[RISC-V Boot Protocol 头文件](https://github.com/tianocore/edk2/blob/edk2-stable202408/UefiCpuPkg/Include/Protocol/RiscVBootProtocol.h) 及 [Linux RISC-V 启动要求](https://docs.kernel.org/arch/riscv/boot.html)。固定 Rust 的目标清单和现有 someboot PE/ELF 路径共同约束产物策略。

已验证原生 RISC-V loader、SNP 传输、标准 Serial I/O 身份、外置 DTB 标准 fixup、someboot EFI 接收和配套服务端。实体 U 盘启动已完成“UEFI → axloader → 网络 ELF/initramfs → 单核 ArceOS”，本次启动链未经过 GRUB 或 Linkr；另已通过板载 GRUB 临时加载 axloader 的 `cargo xtask arceos board` 路径。

结果限于当前 BananaPi 固件、匹配 DTB 和单核 ArceOS。其他目标内核、SMP、AIA、OTA trial/确认/回滚、独立取消后再启动及重复冷启动稳定性仍需分别验收。Linkr 1.3.2 与 1.6.0 虚拟盘曾在固件 USB 枚举阶段出现 UsbKbDxe 错误；该问题尚未解决，不能将实体 U 盘成功解释为虚拟盘已通过。

### 5.3 实体 U 盘启动

使用独立测试 U 盘的 FAT ESP，不覆盖板载系统。`Fdt::prepare()` 只从当前 LoadedImage 的文件系统读取 `board.dtb`，因此 loader 与匹配 DTB 应部署在同一 ESP。

1. 使用 `cargo xtask axloader build --target riscv64-unknown-uefi --release` 构建原生 EFI。
2. 将 `target/riscv64-unknown-uefi/release/axloader.efi` 放为 `EFI/BOOT/BOOTRISCV64.EFI`，将与当前固件匹配的 DTB 放为 `EFI/BOOT/board.dtb`，同步并卸载测试盘。
3. 通过一次性 Boot Manager 进入 UEFI Shell，运行 `map -r` 确认实际文件系统编号，执行该盘的 `\EFI\BOOT\BOOTRISCV64.EFI`；不要固定假设编号为 `FS3`。
4. 等待 `loader_fdt_installed` 和设备发现，释放调试串口，让 ostool 会话完成身份匹配并传输内核与归档。
5. 检查接收内核的四个成功标记，不能只以 loader ready、上传完成或 `ready_to_handoff` 判定通过。

构建与验证使用 `cargo xtask`：宿主契约入口为 `cargo xtask axloader test host`，受影响包用 `cargo xtask clippy --package <package>`。运行真实应用时使用 `cargo xtask arceos board --package arceos-helloworld`，资源和归档验收复用 `boot-resources-smoke` 与 `initramfs-smoke`，板卡配置指向实际 ostool-server 并启用 RISC-V EFI 交接。

`ostool board connect` 在会话就绪后自动开机；HTTPboot 身份等待超时会释放会话并关机。首次固件调试可用带心跳的租约和无人占用的固件串口，执行 axloader 前释放串口供自动绑定。板载 GRUB 也可临时加载 loader 和匹配 DTB；Linux 暂存后应正常重启并核对文件可读，不以直接断电代替卸载。

### 5.4 实板结果

2026-11-04 清理后的版本通过 `cargo xtask arceos board --package arceos-helloworld` 完成实体 FAT32 U 盘冷启动验证，任务退出码为零。UEFI Shell 直接执行 U 盘中的 EFI，记录 `loader_fdt_installed: source=EFI/BOOT/board.dtb`，随后完成自动串口绑定、网络内核与归档传输、EFI 交接及 ArceOS 运行时控制台接管。内核输出 `BOOT_RESOURCES_PASSED`、`HOST_CMDLINE: axloader-positive=1`、`HOST_INITRAMFS_PASSED` 和 `Hello, world!`；该记录证明一次完整链路通过。测试结束后租约回到空闲，继电器读回确认断电，持久板卡配置未变。

已验收产物的 SHA-256 用于对应当次启动日志，后续修改后须重新构建和验证，不能沿用旧摘要宣称新产物已上板。

| 产物 | SHA-256 |
| --- | --- |
| 实体 U 盘 EFI | `3d25395921539a2cd287a352fb96dd03256cd22ebcfbb8d765477392857b4fc9` |
| 固件匹配 DTB | `579a8c0410ed2dd43cf3d6693fde3de2280480466369bde937976682e68e612a` |

清理后的 axloader 宿主契约测试 35 项、someboot qemu-user 测试 86 项及两包定向 Clippy 24 组通过，RISC-V EFI release 构建通过。串口修复此前已通过 RISC-V qemu-user 测试 64 项。接收侧 `no-map` 测试验证全部静态元组保留、RAM 映射排除和必须映射资源冲突，实板也已完成早期页表与运行期内存初始化。详细联调日志另行归档，不将本机临时目录或板卡会话编号作为长期运行配置。

### 5.5 EFI 内存兼容验证

2026-11-05 变基到最新 dev 后，复用设备树校验和保留区映射测试完成两处修复的红绿证明。普通静态保留区由 `BOOT_SERVICES_DATA` 覆盖时，旧校验返回 `UnprotectedReservation`；修复后接受完整覆盖，仍拒绝缺页、可用内存及错误的 `no-map` 类型。EFI RAM 与 FDT 不一致时，旧映射返回 FDT 范围；修复后采用 EFI 范围，并验证 FDT 缺少 `/memory`、没有 FDT、MMIO 排除及 `no-map` 扣除。

`cargo xtask axloader test host` 的 35 项测试、`cargo xtask cross-test --arch x86_64 --package someboot --lib` 的 78 项测试，以及 `cargo xtask clippy --package axloader --package someboot` 的 24 组检查通过。RISC-V EFI loader release 和启用 `boot-resources-smoke,initramfs-smoke` 的 ArceOS 构建通过。RISC-V qemu-user 单测仍因内核链接符号缺失而不能执行，包括 `__someboot_main` 和逐核布局符号；没有通过伪造 ABI 绕过，x86_64 单测验证共享规则，实体启动验证 RISC-V 接收路径。

BananaPi 使用本次构建的内核，经现有实体 U 盘 loader 完成网络 ELF/initramfs 下载与 EFI 交接，四项启动标记全部通过。测试后会话释放、板卡关闭，持久配置未变。原始构建 ELF 的 SHA-256 为 `9194dea602aeb6ef9ede55177b394b0fc087ea08bd1c306809c1d357b72a8748`，实际上传的处理后 ELF 为 `f1df1d94cf9e3ae0eb55a355eac3c288a0b0a85d29abd2b8fe655f7932667a6c`。该轮没有替换 U 盘上的 loader，因此这一实板结果只覆盖新版接收内核；当时新 loader 的保留区校验以红绿测试及原生构建为证，其 EFI 产物 SHA-256 为 `50565c76422b6e4324b0af20dae1db424bccf30ea573bfb6621be805e8b73231`。随后模块拆分后的 loader 已完成 5.6 节的部署和实板验证。

### 5.6 架构模块拆分实板验证

`loader/arch/mod.rs` 按编译目标选择 `riscv64.rs` 或 `x86_64.rs` 的 `run()`，公共 EFI 入口只调用 `arch::run()`。2026-11-05 将拆分后的 RISC-V EFI 产物部署到 BananaPi 的独立测试 U 盘：先在 Linux 确认 USB 父设备、FAT 类型、`AXLOADER` 卷标及分区 GUID，通过会话文件下载新 EFI，核对 SHA-256 后替换 `EFI/BOOT/BOOTRISCV64.EFI`，同步并卸载。原设备树未变，Linux 正常重启回固件后再进行冷启动测试。

UEFI Shell 重新枚举 U 盘并直接执行新 EFI，完成设备树安装、SNP 打开、自动身份绑定、网络 ELF/initramfs 下载、EFI 交接及 ArceOS 控制台接管。实际运行退出码为零，输出 `BOOT_RESOURCES_PASSED`、`HOST_CMDLINE: axloader-positive=1`、`HOST_INITRAMFS_PASSED` 和 `Hello, world!`。测试结束后板卡关闭、租约释放，服务端板卡配置保持不变。

本轮验证把宿主构建产物、Linux 写入后的文件摘要和实际上传内核对应起来，避免把旧 U 盘中的 loader 当作新版验证。

| 产物 | SHA-256 |
| --- | --- |
| 已部署并启动的新 EFI | `36a0b7d5795abff5fa32031a82ab8f4483fbab6c299ceb2969a7672557724734` |
| 原始构建内核 ELF | `fbc7755f80e93a3ba432acde46b43dc6326e963d20c5d61513aba100b6f0494b` |
| 实际上传内核 ELF | `0b5feda4872663ed2caf5f8055af8ee2c7a4ff98034d8629b98aecd662f40eb5` |
| 保持不变的设备树 | `579a8c0410ed2dd43cf3d6693fde3de2280480466369bde937976682e68e612a` |

内核构建使用 `cargo xtask arceos build`。原 `cargo xtask arceos board` 在上传阶段被服务端拒绝，错误为 `kernel ELF architecture Riscv64 does not match declared X86_64`：工作区依赖的 ostool 0.30.3 仍发送错误的架构声明。实板启动因此使用已修复的本地 `ostool board run`，没有修改共享服务或跳过架构校验。该结果证明新版 loader 和接收内核可用，不表示当前锁定依赖下的 `cargo xtask arceos board` 已恢复；后者仍需配套升级客户端。

### 5.7 远端客户端联调

2026-11-06 将工作区的 `ostool` 依赖切换到远端 `main`，由 `Cargo.lock` 固定到 `aa87a8956abde26b44e25edf933eb74006cd7c3d`。此前的 `fix/riscv-efi-boot` 已合并到该提交，原远端分支已删除。该版本的 `HttpBootBoardRunner` 从实际 ELF 获取上传架构；服务端根据架构统一选择 EFI 入口，上传字段中的旧入口名称不构成此次阻塞。

使用该依赖重新编译 xtask 后，`cargo xtask arceos board --package arceos-helloworld` 在 BananaPi 完成“UEFI → 实体 U 盘 axloader → 网络内核及 initramfs → ArceOS”，退出码为零，四项启动标记全部通过。本轮继续使用 5.6 节已部署的 EFI 和匹配 DTB；实际上传内核的 SHA-256 为 `44648843a62c4d5caaf8be7c3da102487ef0b98cd20aa406b41565049ba26e39`。共享服务二进制和持久板卡配置未变，结束后板卡关闭、租约释放。该结果补齐当前锁定依赖下的 xtask 板卡运行验证，其他目标和验收边界仍按 5.2 节说明。
