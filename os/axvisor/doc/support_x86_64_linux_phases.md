# Axvisor x86_64 QEMU Linux 客户机支持工程阶段拆分

## 背景和目标

本文档基于 `os/axvisor/doc/support_x86_64_linux_plan.md` 中已经敲定的方案，将 “Axvisor 在 x86_64 QEMU 平台支持 Linux 客户机” 拆分为可逐步推进、可验证、可回退的工程阶段。

总体目标是在 x86_64 架构上，让 Axvisor 使用 Intel VMX/EPT 方式直接启动 Linux 客户机。第一阶段不追求完整 PC/BIOS 模拟，也不把新增模拟设备作为主要方向，而是参考现有 aarch64/riscv64 Linux direct boot 思路，在 x86_64 上完成 bzImage direct boot、boot_params 构造、initramfs 进入和基础设备直通验证。后续阶段再补齐 timer、中断、PCI、virtio、rootfs 等能力。

## 总体原则

1. 保持 direct boot 路线：Axvisor 直接加载 bzImage、initramfs 和启动元数据，不引入 BIOS/UEFI/完整 PC 固件依赖。
2. 不在第一版 VM 配置中增加 x86_64 专用 `boot_protocol` 字段，优先由 ImageLoader 根据镜像内容自动识别 x86 Linux bzImage。
3. x86_64 Linux 启动元数据使用 Linux x86 boot protocol 的 `boot_params` / zero page，不照搬 aarch64/riscv64 的 DTB-first 路线。
4. Linux 所需的 `console`、`rdinit`、`root` 等启动参数通过 x86 boot protocol 的 `boot_params` command line 传递。aarch64/riscv64 可依赖 FDT `/chosen/bootargs`，但 x86_64 bzImage direct boot 没有 FDT-first 启动元数据，因此 VM config 中的 `kernel.cmdline` 是当前 x86 Linux 路径的启动参数来源。
5. 设备路径以 passthrough 为主。除非 direct boot 或调试闭环必须依赖，否则不优先实现完整模拟串口、模拟块设备或模拟 virtio 设备。
6. 每个阶段都要留下可独立验证的产物，避免把 bzImage 加载、boot_params、VMX/EPT、设备直通、中断路由、rootfs 支持混在一次大改里。

## 阶段 0：现状盘点和最小实验环境

### 阶段目标

确认当前 x86_64 Axvisor 启动链路、QEMU 平台资源、Linux 镜像构建方式和现有配置文件边界，为后续阶段提供稳定的输入。

### 主要任务

1. 梳理现有 x86_64 VM 启动路径：
   - 阅读 `os/axvisor/src/vmm/images/mod.rs`，确认 x86 当前 kernel image、ramdisk、boot image 的加载流程。
   - 阅读 `os/axvisor/src/vmm/images/x86/multiboot.rs`，确认当前 boot stub 的 Multiboot 假设。
   - 阅读 `components/axvm/src/vm.rs` 和 x86 vCPU 创建逻辑，确认 vCPU 初始入口、寄存器、段状态、内存映射来源。
   - 阅读 x86_64 VMExit、EPT、PIO/MMIO 处理路径，确认 passthrough 设备访问会走到哪里。
2. 固定 QEMU 实验输入：
   - 准备一个可重复构建的 x86_64 Linux bzImage。
   - 准备一个最小 initramfs，包含 `/init`，只做串口输出和基本命令验证。
   - 准备一个 QEMU x86_64 Axvisor 配置样例，先只保留内存、vCPU、kernel、ramdisk 和必要 passthrough 资源。
3. 确认 early console 策略：
   - 第一选择是内核内建 command line 中配置 `console=ttyS0` 或等价 early console。
   - 如果使用 initramfs 默认行为，需要确认 `/init` 不依赖 rootfs、PCI、网络和复杂用户态。
4. 记录 QEMU 平台资源：
   - COM1 PIO 地址。
   - Local APIC、IOAPIC、HPET/PIT/RTC 相关地址或端口。
   - PCI config space PIO 地址。
   - 后续 virtio-blk、virtio-console 或其他设备的 BAR 和中断资源。

### 产物

1. 一组可复现的 Linux bzImage 和 initramfs 构建说明。
2. 一个 x86_64 Linux VM 的初始配置样例。
3. 一份 QEMU 平台 passthrough 地址和中断资源记录。

### 验收标准

1. 当前 x86_64 非 Linux 客户机路径仍可运行。
2. 能明确区分现有 Multiboot 启动路径和新增 Linux direct boot 路径。
3. 能在不修改核心代码的情况下，复现实验镜像和 VM 配置输入。

## 阶段 1：x86 Linux bzImage 识别和 header 解析

### 阶段目标

让 ImageLoader 能识别 x86 Linux bzImage，并解析后续 direct boot 所需的 Linux x86 setup header 字段。

### 主要任务

1. 新增 x86 Linux header 解析模块：
   - 建议新增 `os/axvisor/src/vmm/images/x86/linux.rs`。
   - 封装 Linux x86 boot protocol 中需要读取的 setup header 字段。
   - 提供安全的 offset 读取 helper，避免在 loader 主流程里散落 magic offset。
2. 完成 bzImage 合法性校验：
   - 校验 `boot_flag == 0xaa55`。
   - 校验 `header == "HdrS"`。
   - 解析 `setup_sects`，注意 `setup_sects == 0` 时按协议代表 4 个 setup sector。
   - 读取 `version`，明确当前支持的最低 boot protocol 版本。
3. 解析第一阶段必需字段：
   - `code32_start`
   - `cmdline_size`
   - `initrd_addr_max`
   - `kernel_alignment`
   - `relocatable_kernel`
   - `loadflags`
   - `heap_end_ptr`
4. 在 `os/axvisor/src/vmm/images/mod.rs` 中加入自动识别逻辑：
   - x86_64 下先尝试解析 bzImage header。
   - 如果校验通过，进入 Linux direct boot 分支。
   - 如果校验失败，保持现有 Multiboot 路径，继续支持 ArceOS/NimbOS 等客户机。
   - `image_location = "memory"` 和 `image_location = "fs"` 使用同一套识别逻辑。

### 产物

1. `X86LinuxHeader` 或等价结构体。
2. bzImage header 解析和校验单元测试。
3. ImageLoader 中清晰的 Linux / 非 Linux 分流。

### 验收标准

1. 给定合法 bzImage，loader 能识别为 x86 Linux image。
2. 给定现有非 Linux x86 image，loader 不误判，仍走原有 Multiboot 路径。
3. header 解析失败时返回可定位原因的错误，而不是 panic 或静默降级。

### 风险和边界

1. 不在本阶段真正启动 Linux。
2. 不在本阶段引入 VM config boot protocol 字段。
3. 如果遇到不同 Linux 版本 header 字段差异，应先按照 boot protocol 版本收敛支持范围，再扩展兼容性。

## 阶段 2：bzImage payload、initramfs 和 guest 内存布局

### 阶段目标

完成 x86 Linux direct boot 所需的 guest RAM 布局，把 protected-mode kernel payload、initramfs、boot_params、boot stub 放入不冲突的位置。

### 主要任务

1. 明确 guest 物理地址布局：
   - boot stub 放在低地址区域，便于 real mode 入口使用。
   - `boot_params` / zero page 放在 Linux boot protocol 推荐的低地址区域。
   - protected-mode kernel payload 放到 `kernel_load_addr` 或 header 指定/修补后的地址。
   - initramfs 放到 `ramdisk_load_addr`，并确保不超过 `initrd_addr_max`。
   - 保留 E820、APIC、IOAPIC、HPET、PCI MMIO 等区域。
2. 实现 bzImage payload 加载：
   - 根据 `setup_sects` 计算 protected-mode payload offset。
   - 将 payload 拷贝到 guest RAM 的目标 GPA。
   - 处理 `code32_start` 与实际加载地址之间的关系。
3. 实现 initramfs 加载约束：
   - 检查 initramfs 起止地址是否与 kernel、boot_params、boot stub、保留区域冲突。
   - 检查 initramfs end 是否超过 Linux header 中允许的最大地址。
   - 记录 initrd start/size，供 boot_params 阶段写入。
4. 保持现有镜像加载兼容性：
   - Linux direct boot 分支只改变 x86 Linux bzImage 的加载方式。
   - 非 Linux x86 image 不受影响。

### 产物

1. x86 Linux guest memory layout 常量或配置结构。
2. bzImage protected-mode payload 加载逻辑。
3. initramfs 地址合法性检查。
4. 可打印的布局日志，便于启动失败时定位地址冲突。

### 验收标准

1. loader 能把 bzImage payload、initramfs、boot_params 预留区、boot stub 放到预期 GPA。
2. 地址冲突、越界、initrd 超限时能给出明确错误。
3. 现有 x86 非 Linux 客户机启动路径不回归。

### 风险和边界

1. 本阶段可以先使用固定默认地址，后续再参数化。
2. 不在本阶段解决完整 e820 和 Linux 启动寄存器问题。
3. 如果 payload 实际入口需要和 `code32_start` 修补联动，应优先保持 Linux boot protocol 要求一致。

## 阶段 3：boot_params / zero page 构造

### 阶段目标

为 x86 Linux 构造符合 Linux boot protocol 的 `boot_params`，替代 aarch64/riscv64 Linux 路径中的 FDT。

### 主要任务

1. 新增 boot params 构造模块：
   - 建议新增 `os/axvisor/src/vmm/images/x86/boot_params.rs`。
   - 定义 `BootParamsBuilder` 或等价构造器。
   - 将字段写入集中封装，避免调用方直接操作裸 offset。
2. 拷贝并修补 setup header：
   - 将 bzImage setup header 中 Linux 需要保留的部分写入 `boot_params.hdr`。
   - 设置 loader type。
   - 设置 heap 相关字段。
   - 设置必要的 load flags。
   - 根据 direct boot 布局修补 `code32_start` 或相关入口字段。
3. 构造 e820 memory map：
   - 至少描述 guest RAM 可用区域。
   - 标记低地址 boot stub、boot_params 等保留区域。
   - 标记 APIC、IOAPIC、HPET、PCI MMIO 等设备区域为 reserved。
   - 保证 e820 entry 数量和排序满足 Linux 期望。
4. 写入 initrd 信息：
   - 写 `ramdisk_image`。
   - 写 `ramdisk_size`。
   - 如需要，写 ext ramdisk 相关字段以支持高地址 initrd。
5. 处理 command line 策略：
   - 第一阶段默认不从 VM config 写 command line。
   - 如果 Linux header 要求命令行指针字段保持有效，应写入空字符串或最小占位，并记录策略。

### 产物

1. `x86/boot_params.rs`。
2. boot params 字段写入测试。
3. e820 map 构造测试。
4. loader 调用 boot params builder 的集成逻辑。

### 验收标准

1. guest RAM 中生成的 `boot_params` 能被离线检查工具或测试断言验证。
2. e820 至少包含 RAM 和 reserved 区域，且不会把设备 MMIO 当作普通 RAM 暴露给 Linux。
3. initrd start/size 与实际加载地址一致。

### 风险和边界

1. e820 不完整会导致 Linux 早期内存管理异常，应优先把保守 reserved 区域写正确。
2. 不建议在此阶段引入复杂 ACPI/MPTable 模拟。
3. command line 策略如果阻塞 early console，应优先调整镜像构建，而不是扩大 VM config 语义。

## 阶段 4：x86 Linux boot stub 和 vCPU 初始状态

### 阶段目标

新增 Linux 专用 x86 boot stub，使 vCPU 能从 Axvisor 的初始入口进入 Linux boot protocol，并跳转到 Linux kernel 入口。

### 主要任务

1. 拆分或扩展 x86 boot stub：
   - 保留现有 `DEFAULT_BIOS_IMAGE` 或 Multiboot stub，继续服务 ArceOS/NimbOS。
   - 新增 `DEFAULT_LINUX_BOOT_IMAGE`，或拆出 `x86/linux_boot.rs`。
   - Linux boot stub 不影响非 Linux x86 image。
2. 实现第一版保守 real-mode 启动：
   - 设置基础段寄存器。
   - 设置栈。
   - 准备 Linux boot protocol 所需寄存器。
   - 将 `boot_params` 地址传递给 Linux。
   - 跳转到 Linux setup 或 protected-mode entry。
3. 调整 vCPU 初始 config：
   - Linux direct boot 分支使用 Linux boot stub 入口。
   - 非 Linux 分支继续使用现有入口。
   - 检查 real mode 下 CS:IP、段基址、CR0、EFER 等状态是否符合 stub 假设。
4. 加强启动日志：
   - 打印 Linux direct boot 入口地址。
   - 打印 boot_params GPA。
   - 打印 kernel payload GPA 和 initramfs GPA。

### 产物

1. Linux 专用 x86 boot stub。
2. x86 vCPU 初始状态分流逻辑。
3. boot stub 与 boot_params 的集成测试或最小启动日志。

### 验收标准

1. vCPU 能进入 Linux boot stub。
2. boot stub 能跳转到 Linux 入口，不再停留在 Axvisor 侧启动代码。
3. 非 Linux x86 客户机继续走原有 Multiboot stub。

### 风险和边界

1. real-mode stub 对段状态和内存位置敏感，应优先保持实现极小。
2. 如果 real-mode 路线遇到难以处理的问题，可记录后切换 PVH/direct protected-mode 方案，但不应在没有证据前扩大范围。
3. 本阶段只保证进入 Linux 早期入口，不要求看到完整用户态。

## 阶段 5：early serial console 和 initramfs 最小闭环

### 阶段目标

让 Linux 在 Axvisor x86_64 QEMU VM 中输出 early boot log，并进入 initramfs `/init`，形成第一个可观察的 Linux 客户机闭环。

### 主要任务

1. 打通串口或 console 输出：
   - 优先直通 QEMU COM1 PIO 区域。
   - 确认 PIO 访问能从 guest 转发到 host/QEMU。
   - 确认 Linux 内建 command line 或 initramfs 配置会把输出写到期望 console。
2. 验证 boot_params 对 Linux 早期启动足够：
   - Linux 能解析 e820。
   - Linux 能识别 initrd。
   - Linux 不因为 command line、loader type、heap、load flags 等字段缺失而提前崩溃。
3. 验证 initramfs `/init`：
   - `/init` 输出明确的启动成功标记。
   - `/init` 可以执行基础 busybox 或 shell 命令。
   - `/init` 不依赖 block rootfs。
4. 收集失败信息：
   - Axvisor VMExit 日志。
   - Linux early console 日志。
   - EPT fault / PIO fault / triple fault 相关信息。

### 产物

1. 可进入 initramfs 的 x86_64 Linux VM 配置。
2. 一份最小 initramfs 验证脚本或构建说明。
3. early boot 成功日志样例。

### 验收标准

1. Linux 能输出 early boot log。
2. Linux 能挂载 initramfs 并执行 `/init`。
3. `/init` 输出稳定成功标记，可用于自动化匹配。

### 风险和边界

1. 本阶段不要求挂载真实 rootfs。
2. 本阶段不要求 PCI/virtio 完整工作。
3. 如果 timer 或中断缺失阻塞 initramfs，需要把最小 timer/interrupt 修复提前纳入本阶段。

## 阶段 6：配置对齐和 APIC/topology 暴露收敛

### 阶段目标

在阶段 5 已经能进入 initramfs `/init` 的基础上，先把 x86_64 Linux 客户机配置与其他平台 Linux 客户机对齐，并让单 vCPU、禁 APIC bring-up 路径的 CPUID 暴露与实际能力一致。更完整的 APIC、timer、IRQ 和 CPU topology 支持后移到单独阶段。

### 主要任务

1. 对齐 Linux VM 配置语义：
   - 参考 aarch64 / riscv64 Linux 客户机配置，将 x86_64 Linux `vm_type` 对齐为 Linux 类型。
   - 默认内核继续使用已验证能进入 `/init` 的 Ubuntu `bzImage`。
   - `emu_devices` 保持为空，第一版不默认引入更多 x86 legacy PIO 设备模型。
2. 收敛单 vCPU CPUID 暴露：
   - 确认 CPUID 中 APIC、x2APIC、CPU topology 相关 leaf 暴露与实际虚拟平台一致。
   - 隐藏当前未支撑的 local APIC、x2APIC 和 TSC deadline timer 能力。
   - 在单 vCPU 场景提供一致的 logical processor count 和 initial APIC ID。
   - 让 CPUID topology leaf `0xb` / `0x1f` 与当前无拓扑暴露能力一致。
3. 记录 `linux-qemu` 后续切换条件：
   - 当前阶段 5 默认使用 Ubuntu `bzImage`，因为它已经能在 Axvisor 下进入 `/init`。
   - `linux-qemu` 已确认是合法 bzImage，裸 QEMU 能进入 `/init`；阶段 6 CPUID 收敛后已经绕过 APIC/topology panic，但仍停在 8250 串口驱动初始化附近。
   - 避免 guest legacy PIO 设备直接复用已经被 Axvisor host 初始化改写过的 PIC/PIT/串口状态。
   - 后续如需支持 `linux-qemu`，再单独补 PIC/PIT legacy 路径或 APIC/timer/IRQ 路径。

### 产物

1. x86_64 Linux VM 配置与其他平台 Linux 客户机对齐。
2. 单 vCPU、禁 APIC bring-up 的 CPUID 暴露收敛记录。
3. `linux-qemu` 阶段性验证记录和后续切换条件。
4. 默认 `bzImage` initramfs smoke test 记录。

### 验收标准

1. 默认 x86_64 Linux 配置仍能进入 initramfs `/init` 并输出 marker。
2. `vm_type` 与其他 Linux 客户机配置语义一致。
3. CPUID 不再向当前默认路径暴露未支撑的 APIC、x2APIC、TSC deadline 和 topology 能力。
4. `linux-qemu` 的剩余阻塞点和后续补齐方向有明确记录。

### 风险和边界

1. 本阶段不实现完整 APIC、IOAPIC、PIC、PIT、HPET 或串口模拟设备。
2. 本阶段不把 `linux-qemu` 切为默认内核。
3. x86 平台中断链路容易牵连 APIC、IOAPIC、PIC、MSI；后续补齐时应单独拆分并坚持最小闭环优先。
4. 如果 passthrough 与虚拟化隔离冲突，应记录原因，再决定是否实现半直通控制器。

## 阶段 7：PCI config space 和 virtio / block 设备直通

### 阶段目标

让 Linux 能发现并使用后续 rootfs 所需的 PCI/virtio/block 设备，为从 initramfs 过渡到真实 rootfs 做准备。阶段 7 先把 PCI config、low MMIO window 和 virtio-blk 设备发现合并进默认配置，再继续处理 block I/O completion 和中断投递。

### 主要任务

1. 合并 PCI 实验到默认配置：
   - 默认 `linux-x86_64-qemu-smp1.toml` 去掉 `pci=off`，改为 `pci=conf1`。
   - 删除临时 `linux-x86_64-qemu-smp1-pci.toml`。
   - 记录 QEMU 已暴露的 `virtio-blk-pci` 和 guest 当前消费它的阶段性结果。
2. 梳理并验证 PCI config space：
   - 支持或直通 `0xcf8/0xcfc` PIO 访问。
   - 当前 VMX I/O bitmap 默认主要是 passthrough 策略，需确认直接落到外层 QEMU/host PCI config 状态是否可接受。
   - 确认 Linux 能枚举 QEMU 暴露的 PCI host bridge 和设备。
   - 处理 PCI BAR 读取、写入和资源分配行为。
3. 打通设备 BAR passthrough：
   - 将 virtio-blk、virtio-console 或目标 block 设备 BAR 映射到 guest。
   - 确认 EPT 映射属性适合 MMIO。
   - 确认 PIO BAR 和 MMIO BAR 都能被访问。
   - PCI BAR / ECAM / PCI MMIO window 如由配置引入，需要同步写入 E820 reserved。
   - 默认配置已加入 `0xfe00_0000..0xfec0_0000` low MMIO passthrough，Linux 能枚举到 `virtio_blk virtio0` 和 `vda`。
4. 打通 DMA 访问：
   - 确认设备 DMA 使用的 GPA/HPA 映射关系。
   - 如果需要 IOMMU 或 bounce buffer，明确最小实现策略。
   - 确认 Linux virtqueue descriptor、avail、used ring 能被设备正确访问。
5. 打通设备中断：
   - 优先确认 legacy INTx 路径。
   - 后续再扩展 MSI/MSI-X。
   - 将设备中断注入到正确 vCPU。

### 产物

1. 默认 x86_64 Linux VM 配置中的 PCI/virtio-blk 设备发现路径。
2. PCI config space passthrough 或半直通验证记录。
3. virtio/block 设备 BAR 和 DMA 路径验证。
4. initramfs block 只读 smoke test。

### 验收标准

1. 默认配置能进入 PCI 枚举路径。
2. 默认路径能明确记录 Linux 停在哪个 PCI / BAR / DMA / IRQ 阶段。
3. Linux `lspci` 或启动日志能看到目标 PCI/virtio 设备。
4. Linux 能加载对应驱动，并识别 virtio block 设备。
5. initramfs 内能读写目标 block 设备。

### 风险和边界

1. PCI 和 DMA 问题通常会暴露 EPT、缓存属性和地址翻译问题，应单独记录每类 fault。
2. 默认配置已经打开 PCI，若 `/init` 暂时回退，应优先定位 virtio-blk I/O completion / IRQ，而不是恢复临时 PCI 配置。
3. 本阶段仍不要求完整 rootfs 启动成功，但必须为 rootfs 阶段消除设备发现和基础 I/O 障碍。

## 阶段 8：真实 rootfs 启动

### 阶段目标

让 x86_64 Linux 客户机从 initramfs 过渡到真实 rootfs，完成可交互或可自动化验证的用户态启动。

### 主要任务

1. 准备 rootfs 镜像：
   - 使用 ext4、initramfs pivot_root、initrd handoff 或其他明确方式挂载 rootfs。
   - rootfs 中包含基础 shell、mount、dmesg、lspci、block 工具。
   - rootfs 的 kernel module 策略与内核配置匹配。
2. 解决启动参数来源：
   - 优先通过内核内建 command line 指定 `root=`、`console=`、`rdinit=` 等。
   - 如果内建 command line 不足，再评估是否需要在 boot_params 中写 command line。
   - 不把 VM config 扩展作为默认方案。
3. 验证 rootfs 挂载和 init：
   - Linux 能识别 root block 设备。
   - Linux 能挂载 rootfs。
   - `/sbin/init` 或指定 init 能启动。
4. 建立自动化成功标记：
   - rootfs 启动后输出固定字符串。
   - 可选执行基础文件系统读写测试。
   - 可选执行网络或多进程 smoke test。

### 产物

1. 可启动 rootfs 镜像。
2. x86_64 Linux rootfs VM 配置样例。
3. rootfs 启动成功日志。

### 验收标准

1. Linux 能从真实 rootfs 启动到用户态。
2. block 设备读写稳定。
3. 启动成功标记可被 CI 或本地脚本识别。

### 风险和边界

1. rootfs 启动失败可能来自内核配置、命令行、PCI/virtio、DMA、中断或文件系统本身，需要按阶段回溯定位。
2. 不在本阶段追求多设备、多队列、高性能。

## 阶段 9：单 vCPU 平台收敛、稳定性和回归测试

### 阶段目标

在单 vCPU Linux 客户机可用后，继续收敛 timer/clocksource、IRQ routing、PCI/设备拓扑、
rootfs/init 和回归测试，保证 x86 Linux direct boot 不靠不断增加启动参数维持可用性。阶段 9
不实现多 vCPU，默认继续限制为 `cpu_num = 1`。

### 主要任务

1. 收敛 x86 timer/clocksource：
   - 已补最小 PIT/8254 port 设备、VMX I/O bitmap 捕获、VMX preemption timer 驱动的
     `VTimer` exit，以及经 vIOAPIC GSI 0 注入的 IRQ0 路径。
   - 已去掉 `no_timer_check` 和 `pmtmr=0x608`；继续收敛 `tsc=unstable`。
   - 后续补完整 8259/PIC、Virtual Wire/ExtINT、LAPIC timer 和 PIT 边角语义。
   - 记录每个参数移除失败时的真实卡点。
2. 收敛 IRQ routing：
   - 将 q35 virtio-blk GSI 23 hardcode 推进为基于 vIOAPIC redirection table 的通用 GSI forwarding。
   - 继续补 PCI INTx routing，后续再评估 MSI/MSI-X。
3. 收敛 QEMU 设备拓扑：
   - 通用 `qemu-x86_64.toml` 保持兼容 NimbOS / ArceOS。
   - Linux 专属最小拓扑使用独立 QEMU 配置，避免默认 VGA、e1000、ICH/AHCI 等设备拖慢 Linux 探测。
   - Linux 专属 board 不启用 host `fs`，x86 Linux kernel 走 `image_location = "memory"`，
     避免 Axvisor host 与 Linux guest 同时挂载同一块 ext4 rootfs。
4. 收敛 rootfs/init：
   - 默认 cmdline 已切到 BusyBox getty 拉起 `/bin/sh`，避免裸 `init=/bin/sh` 缺少
     controlling tty。
   - Linux 专用 QEMU 模板禁用 stdio monitor 复用，避免交互 shell 误触 monitor escape。
   - 完整 `/sbin/init` / OpenRC 支持后续再修，不阻塞最小 Linux shell。
5. 稳定性测试：
   - 重复启动 initramfs VM。
   - 重复启动 rootfs VM。
   - 执行基础 CPU、内存、文件系统 I/O 压力。
6. 回归测试：
   - 验证现有 x86 非 Linux 客户机。
   - 验证 aarch64/riscv64 Linux direct boot 不受 x86 改动影响。
   - 验证 image loader 对不同 image 类型的分流。
7. 文档和配置沉淀：
   - 更新 qemu quickstart 或新增 x86_64 Linux quickstart。
   - 补充 VM config 示例。
   - 记录已知限制和排错方式。

### 产物

1. 单 vCPU 稳定性报告。
2. timer/clocksource 参数移除记录。
3. 回归测试清单。
4. 用户可复现的 quickstart 文档。

### 验收标准

1. Linux initramfs/rootfs 启动可重复。
2. 非 Linux x86 客户机不回归。
3. 新增文档足够让其他开发者复现实验。

### 风险和边界

1. 多 vCPU 已明确移出本阶段；AP startup、LAPIC IPI、SMP topology 不应阻塞阶段 9。
2. 性能优化不是泛化目标，但与 timer、IRQ routing、QEMU 拓扑直接相关的启动慢路径需要处理。
3. 不再优先通过 VM TOML 或 boot params 叠加 workaround；应优先补平台能力。

## 建议实施顺序

推荐按以下顺序推进：

1. 阶段 0：固定实验输入和现状边界。
2. 阶段 1：实现 bzImage 识别和 header 解析。
3. 阶段 2：完成 payload、initramfs 和 guest 内存布局。
4. 阶段 3：生成 boot_params。
5. 阶段 4：接入 Linux boot stub 和 vCPU 初始状态。
6. 阶段 5：跑通 early console 和 initramfs。
7. 阶段 6：补齐 timer、中断和基础平台设备。
8. 阶段 7：打通 PCI/virtio/block 设备直通。
9. 阶段 8：启动真实 rootfs。
10. 阶段 9：单 vCPU 平台收敛、稳定性、回归和文档。

阶段 1 到阶段 4 是 Linux 能进入早期启动的核心链路，建议作为第一组开发 PR。阶段 5 和阶段 6 是第一个可观察闭环，建议作为第二组开发 PR。阶段 7 和阶段 8 涉及设备、DMA、中断和 rootfs，建议拆成多个独立 PR。阶段 9 作为稳定化和产品化阶段，不应和初始 bring-up 混在一起。

## 每阶段通用检查项

1. 修改逻辑后运行相关 `cargo fmt`。
2. 修改 crate 后运行对应 `cargo xtask clippy --package <crate>`，如果目标 crate 尚未被 xtask 支持，再按项目现有方式选择最接近的 clippy 命令。
3. 保持 x86 Linux direct boot 与现有 x86 Multiboot 路径分离。
4. 启动失败时优先记录 GPA/HPA、VMExit reason、EPT fault、PIO/MMIO 地址、Linux early log。
5. 每次引入新设备 passthrough 时，同步更新配置样例和已知限制。
