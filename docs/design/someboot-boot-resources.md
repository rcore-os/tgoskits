# someboot 启动资源与 EFI 交接

## 1. 能力与所有权

someboot 同时服务内核启动和原生 UEFI 应用。`boot_resources` 管理启动期内存，`handoff` 管理交接描述和发布事务；镜像地址、池容量及 OTA 策略属于应用。`boot-resources` 不依赖网络、文件系统或交接生产端，`efi-handoff` 组合 `efi` 与资源能力。普通 EFI 内核消费交接不需要生产端 feature。

### 1.1 资源计划

[`boot_resources`](../../platforms/someboot/src/boot_resources/mod.rs) 的 `declare_boot_resources!` 将 `ResourceRequest` 数组写入保留的 `.someboot.resources` 链接节。请求包含资源 ID、容量、对齐、地址约束和可选的 `InitialContents`，不包含应用函数或静态指针。`prepare_static_plan()` 在退出 Boot Services 前校验整个计划、申请资源并初始化内容；失败时逆序释放本轮全部申请，随后终止这次必需资源初始化，不发布部分计划。没有声明时不申请应用池。原生 UEFI 应用通过 `reserve()` 执行相同的类型化请求，错误直接返回调用方。

[`allocation.rs`](../../platforms/someboot/src/boot_resources/allocation.rs) 的 `Allocation` 是唯一所有者。预留区通过原子水位分配，逆序释放以 CAS 回退水位，非逆序释放保留空间直到重启，避免旧句柄覆盖后续分配。`ResourceSnapshot` 保存 ID、范围、不可变前缀 `initialized` 和分配水位 `used`，并要求 `initialized <= used <= capacity`。分配仅使用前缀之后的空间；继承阶段在启动处理器释放次处理器前校验本镜像的静态计划是否被快照满足，再恢复全部水位，不重新清零，也不复用祖先或当前目标已经占有的空间。缺失资源或不兼容的地址、容量和对齐会拒绝启动。

`InitialContents::FirmwareBootImage` 显式请求在固件仍可读文件时保存最初启动的 EFI 文件。继承阶段继续得到同一份固件来源文件，不能把它解释为当前运行子镜像。`firmware::initialize_image()` 使用固件分配的单页缓冲读文件，再复制到内核直接映射地址，避免把内核虚拟地址交给固件 DMA。文件不可访问或大于请求容量时，本次必需计划失败；不从已重定位、数据段已改变的运行镜像伪造可持久化文件。

### 1.2 构建边界

[`lib.rs`](../../platforms/someboot/src/lib.rs)、[`Cargo.toml`](../../platforms/someboot/Cargo.toml) 与 [`build.rs`](../../platforms/someboot/build.rs) 共同以 `target_os = "uefi"` 隔离内核入口、页表、SMP 及其依赖，原生构建只保留公共能力和固件适配，也不生成内核链接脚本。其他目标继续使用原有根模块和符号路径，构建脚本派生的 `cfg(efi)` 保持原语义。构建模式由 target 确定，不使用反向 feature，避免 Cargo feature 合并改变产物模式。

## 2. EFI 交接

[`handoff`](../../platforms/someboot/src/handoff/mod.rs) 的 `HandoffSpec` 描述目标镜像物理范围、cmdline、initramfs 和带 ID、版本的应用元数据。通用验证先于平台发布；具体 `PreparedHandoff` 守卫在目标返回时撤销交接。调用方保留镜像分配、关闭网络和设备服务后，才能调用其非安全 `enter()`。LoongArch 运行于 Boot Services 已退出的环境，原生 x86 UEFI 在固件服务仍有效时调用目标，两者保留不同的清理边界。

### 2.1 协议与继承

[`protocol::HandoffRecord`](../../platforms/someboot/src/handoff/protocol.rs) 使用 v3 和 GUID `7b9fa931-a25b-d043-b13c-63435428c473`，采用 `repr(C)`、固定宽度整数及有界数组：最多 8 个资源、128 个内存区、4095 字节 ASCII cmdline、512 字节应用元数据。它不保存 OTA 槽位或固定池。`boot_services_exited` 区分固件活动与继承阶段：前者不替代固件内存图，也不携带资源或归档；后者必须验证全部资源范围由 Reserved/KImage 内存完整覆盖，镜像与归档位于已分配且不属于不可变前缀的范围内，并验证区间、版本和边界。

LoongArch 的 [`handoff/received/loongarch64.rs`](../../platforms/someboot/src/handoff/received/loongarch64.rs) 在复用固件配置表之前，将完整记录复制到本镜像的私有存储，并负责继承资源及内存图的恢复；通用元数据副本属于 `handoff/received`，`handoff::inherited_metadata()` 不返回可被下一次生产事务改写的共享表引用。`boot_payload` 只负责 initramfs 的发布、领取及回收。`efi_stub/loongarch64/mod.rs` 只转换固件表存储地址，函数指针保持原值；目标恢复内存图并跳过第二次 ExitBootServices。普通 EFI 消费端始终随 `cfg(efi)` 编译，资源恢复仅要求 `boot-resources`，与生产端 `efi-handoff` 独立。

v2 与 v3 不兼容。LoongArch 遇到旧 GUID 明确拒绝；生产者、目标和 A/B 槽必须在本次迁移中一起重建，不承诺旧二进制槽与新协议互相交接。持久 `State` 记录格式不变，但不能靠回滚至旧协议 loader 绕过这个重建要求。

### 2.2 固件路径

x86 的 [`handoff/firmware`](../../platforms/someboot/src/handoff/firmware/mod.rs) 保留 LoadedImage LoadOptions、Linux LoadFile2 的查询和复制语义，元数据另以 v3 配置表发布；OTA 仍由 axloader 的 LoadImage/StartImage 路径拥有。私有 [`uefi_protocol`](../../platforms/someboot/src/handoff/uefi_protocol/mod.rs) 承载不调用固件的 UCS-2 编码、设备路径和 LoadFile2 回调，宿主测试只编译这一层；安装、撤销和事务守卫仅由原生 UEFI 后端装配。目标返回且固件服务仍活动时恢复原 LoadOptions 和原配置表、卸载 initrd 提供者并释放分配；若目标已经退出服务，守卫保留这些页，不再调用 Boot Services。配置表或协议撤销失败时也不能释放仍被固件引用的内存。

LoongArch 的 [`efi_stub/loongarch64/handoff.rs`](../../platforms/someboot/src/efi_stub/loongarch64/handoff.rs) 在首次退出前安装配置表，之后只操作预留资源、复制 cmdline/归档/元数据、发布快照并刷新指令缓存。事务守卫排除重复发布，失败或目标返回时撤销记录并回退本事务拥有的归档分配。`someboot::efi_stub::setup_service` 和 `someboot::boot_payload::publish` 路径保持不变；initramfs 继续一次领取，预留区中的归档不作为独立固件页回收。

### 2.3 应用策略

[`axstd_loader`](../../bootloader/axloader/src/axstd_loader.rs) 显式声明 4 项资源：32 MiB 镜像区固定在 `0x200000`，256 MiB 归档区、32 MiB EFI 槽执行区，以及 32 MiB 固件原文件区。原文件区用于初次验证、Flush 并读回 `A.EFI`，不保存运行时已修改的数据段，因此未确认 trial 复位后能重新执行稳定槽。该配置要求最初 loader 由可读的 EFI 文件系统启动，文件不得超过声明容量。继承槽必须已有有效持久状态；状态缺失时拒绝把祖先文件冒充当前槽重新建档。

[`ota/handoff.rs`](../../bootloader/axloader/src/ota/handoff.rs) 编解码 axloader 自己的元数据 ID、版本、槽位和持久代次；someboot 只承载有界字节。固定镜像地址属于应用策略：若当前镜像已经占有相同执行区，则拒绝再次覆盖该区，不猜测 load bias。原生 x86 ELF 路径也使用共同 `ResourceRequest`/`reserve()` 和 `HandoffSpec`/`PreparedHandoff`，仍由固件决定实际页申请与 EFI 生命周期。

axloader 的 [`http_control`](../../bootloader/axloader/src/http_control.rs) 统一路由分类、单值控制头、epoch 和 trial 准入；状态查询不要求 epoch，修改请求仍核对当前代次。两前端仅保留传输与平台动作。共同 [`elf::ElfImage`](../../bootloader/axloader/src/elf.rs) 校验 ELF64、machine、段范围及可执行入口；x86 保留未对齐段与固件 load bias，LoongArch 后端拥有页对齐、固定地址和缓存策略。someboot 的 `handoff::cmdline` 同时约束生产与消费，4095 字节 ASCII 上限不再各自实现。

### 2.4 运行时退出

LoongArch 的目标会重用继承内存图中的空闲页；关闭 HTTP socket 不能证明旧网络设备已停止 DMA。退出由 [`ax-runtime::handoff::enter`](../../os/arceos/modules/axruntime/src/handoff.rs) 编排，someboot 不依赖网络、文件系统或任务：应用完成 OTA 状态提交、释放连接并准备好交接事务后，调用终结性的运行时交接入口。当前入口只接受一个运行处理器，应用必须先停止自己创建的任务及 I/O；多处理器退出不在本阶段能力内。

退出顺序为文件系统同步与封存、块控制器停止及 IRQ 同步、网络协议任务退出并 join、网络 IRQ 同步及队列任务停止并 join、控制台排空和串口屏蔽，最后关闭本核 IRQ、抢占与 clockevent，再调用目标。块设备复用 `release_block_irqs_for_passthrough()` 已有的完整停稳契约。网络需要把 IRQ 同步、任务 join 与 DMA 停稳结果逐层返回；任何隔离保留只能保护当前内核，不能作为交接成功的证据。VirtIO 网络写入设备状态零后读取状态零，按 VirtIO 1.2 §2.4 确认设备不再访问旧队列，才允许释放队列 backing。

不采用只丢弃 socket 或禁用 CPU IRQ 的方案，因为设备仍可能异步写旧内存。也不在继承内存图中永久保留全部运行时堆来掩盖设备未退出的问题。退出一旦开始不恢复 HTTP 服务；任一步失败、处理器数量不符或目标返回都复位机器，由已经持久化的 OTA 状态处理回滚，不能返回已经停运的运行时。目标入口、地址转换和缓存维护继续归 someboot 平台适配层。

目标在开启中断、接管异常或复用祖先运行时内存前，先安装自己的异常入口、处理器局部状态和栈。返回式 EFI 程序必须保持调用方的栈、MMU 映射、处理器局部状态和中断屏蔽；已经替换这些执行上下文的内核不得返回旧 EFI 调用帧。LoongArch smoke 上传仅执行 `ori $a0, $zero, 0; ret` 的独立 ELF，要求入口返回后机器复位，重新提供具有新 epoch 的 HTTP 服务，并保持稳定 OTA 状态；它与正常内核交接分别验证返回和不返回两条路径。

## 3. 迁移与验证

所有生产端与消费端在同一 PR 重建，不保留 `efi-loader` 或旧 `loader_*` 别名。公共算法使用宿主测试验证，目标地址转换、缓存和固件生命周期通过真实 EFI QEMU 验证。

### 3.1 共同性与差异

共同机制按资源与事务分层；涉及固件调用和处理器状态的动作留在选定的平台模块，不增加公共 trait 或动态分派。

| 层次 | 共同机制 | 平台保留职责 |
| --- | --- | --- |
| 资源 | 请求校验、所有权、失败回退、快照 | EFI 页申请、直接映射 |
| 交接 | 描述校验、ABI、应用元数据、事务 | 配置表与 LoadOptions/LoadFile2 |
| 启动 | 校验目标及活动镜像不重叠 | EFI ABI、缓存、继承内存图 |

只重命名旧 feature 无法移走固定三池和 OTA 字段；新增独立软件包则会拆散 someboot 已有的启动资源生命周期，因此采用现有软件包内的公共模块。所有功能转发已在 axstd、ax-runtime、ax-hal、axplat-dyn、somehal 和 axloader 同步迁移，删除旧入口而不建立兼容别名。

### 3.2 验收边界

someboot 宿主 profile `boot-resources` 验证计划校验、部分申请回退、继承水位和协议拒绝；原生 UEFI 产物契约检查实际 rlib、依赖与构建脚本输出，确认没有内核入口。Clippy 配置覆盖 x86 裸机、原生 x86 UEFI、AArch64 EFI 编译、RISC-V 非 EFI、LoongArch 普通 EFI、资源及交接生产组合。

`python3 bootloader/axloader/scripts/loong-qemu-smoke.py` 先执行脱离网络、OTA 和交接生产端的资源应用，再在独占 FAT、QMP socket 和宿主端口中验证写满返回 `409 ota_storage_failed` 且状态不变，以及 HTTP 独立 ELF、cmdline/initramfs、未确认 trial 复位回滚、旧确认拒绝、新 trial 确认及再次复位后的稳定摘要。x86 使用 `cargo xtask axloader test qemu --target x86_64-unknown-uefi` 的既有可选 payload 和真实 FAT OTA 回归。宿主模型不能代替这些固件和持久存储证据。AArch64 EFI 镜像生成与 LoongArch 非 EFI 启动不在本轮范围。
