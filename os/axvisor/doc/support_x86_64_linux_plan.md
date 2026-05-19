# Axvisor x86_64 VMX 启动 Linux 客户机方案

## 目标

在 x86_64 架构上，让 Axvisor 使用 Intel VMX/EPT 方式直接启动客户机 Linux。第一阶段目标不是完整模拟 PC/BIOS，而是参考现有 aarch64/riscv64 Linux 启动路径，走 direct kernel boot：

1. Axvisor 直接把 Linux bzImage、initramfs 和启动元数据加载到 guest RAM，不在 VM 配置中传递 kernel command line。
2. vCPU 从一个极小的 x86 boot stub 进入 Linux boot protocol。
3. 先跑到 early serial console 和 initramfs `/init`，验证 VMX、EPT、Linux boot params、设备直通映射和中断转发链路。
4. 后续再扩展 PCI/MSI/MSI-X/IOAPIC/LAPIC 等直通相关能力，支持真实 rootfs。

## 现有 aarch64/riscv64 Linux 启动方式

当前 aarch64/riscv64 的 Linux VM 配置和启动路径基本是 direct boot：

- VM 配置直接指定 `kernel_load_addr` 和 `entry_point`。
- Linux kernel 被加载到 guest RAM 的固定 GPA。
- `dtb_load_addr` 指定 DTB 的加载地址。
- Axvisor 生成或裁剪 guest FDT，写入 memory node、passthrough device node、`/chosen/bootargs`、`linux,initrd-start` 和 `linux,initrd-end`。
- vCPU 创建时按 Linux 启动 ABI 设置寄存器：
  - aarch64: `x0 = dtb_addr`
  - riscv64: `a0 = hartid`, `a1 = dtb_addr`
- 设备主要通过 passthrough 或半直通方式暴露给 guest。

关键代码参考：

- `os/axvisor/src/vmm/images/mod.rs`: 加载 kernel、ramdisk、DTB。
- `os/axvisor/src/vmm/fdt/create.rs`: 生成/修补 guest FDT。
- `components/axvm/src/vm.rs`: 创建架构相关 vCPU config，将 DTB 地址传给 vCPU。
- `components/arm_vcpu/src/vcpu.rs`: aarch64 vCPU 将 DTB 地址写入 `x0`。
- `components/riscv_vcpu/src/vcpu.rs`: riscv64 vCPU 将 hart id / DTB 地址写入 `a0` / `a1`。

## x86_64 不能直接照搬 DTB 路线

x86_64 Linux 的传统 bzImage 启动协议不是 DTB-first。它需要 Linux x86 boot protocol 中定义的 `boot_params` / zero page、initrd 字段、e820 memory map 等信息。

现有 x86_64 路径更接近 Multiboot/裸内核启动：

- `os/axvisor/src/vmm/images/x86/multiboot.rs` 提供一个极小 boot stub。
- `os/axvisor/src/vmm/images/mod.rs` 当前会为 x86 写 Multiboot info。
- `os/axvisor/src/vmm/images/linux.rs` 目前只解析 arm64/riscv Linux header，没有解析 x86 setup header。

因此 x86_64 应该复用 direct boot 的框架思想，但把 “生成 DTB” 替换为 “生成 Linux x86 boot params”。

## 总体设计

### 启动数据映射

<img width="1481" height="447" alt="Image" src="https://github.com/user-attachments/assets/b48e0bf6-43ac-457b-906a-44c62b3d6bcd" />

### 第一阶段启动流程

1. Axvisor 读取 VM config。
2. 加载 bzImage 到 guest RAM。
3. 解析 bzImage x86 setup header。
4. 加载 protected-mode kernel payload 到 `kernel_load_addr`。
5. 加载 initramfs 到 `ramdisk_load_addr`。
6. 构造 `boot_params`：
   - setup header 拷贝/修补
   - e820 memory map
   - initrd start/size
   - loader type、heap/end 等必要字段
7. 加载 x86 Linux boot stub。
8. vCPU 以 real mode 进入 boot stub。
9. boot stub 设置 Linux 需要的寄存器和段状态，跳入 Linux 入口。
10. Linux 通过直通串口或直通 virtio-console 输出 early boot log。
11. Linux 使用 initramfs 进入 `/init`。

## 需要新增或修改的模块

### 1. x86 Linux header 解析

建议新增：

- `os/axvisor/src/vmm/images/x86/linux.rs`

职责：

- 解析 bzImage setup header。
- 校验 `boot_flag == 0xaa55`。
- 校验 `header == "HdrS"`。
- 读取 `version`、`setup_sects`、`code32_start`、`cmdline_size`、`initrd_addr_max`、`kernel_alignment`、`relocatable_kernel` 等字段。
- 提供安全的字段读写 helper，避免到处硬编码 offset。

输出建议：

```rust
pub struct X86LinuxHeader {
    pub setup_sects: usize,
    pub boot_protocol_version: u16,
    pub code32_start: u32,
    pub cmdline_size: u32,
    pub initrd_addr_max: u32,
    pub kernel_alignment: u32,
    pub relocatable_kernel: bool,
}
```

### 2. boot_params / zero page 构造

建议新增：

- `os/axvisor/src/vmm/images/x86/boot_params.rs`

职责：

- 在 guest RAM 中生成 Linux `boot_params`。
- 写 e820 表，至少覆盖普通 RAM 和保留区域。
- 写 initrd 起止信息。
- 写 loader 标识。
- 根据 kernel header 修补 load flags。

初始布局建议：

<img width="1475" height="377" alt="Image" src="https://github.com/user-attachments/assets/e0f14e0b-3dd6-450c-abfd-97da71ecd90f" />
具体地址可以通过 VM config 暴露，默认值先固定，后续再参数化。

### 3. x86 Linux boot stub

现有 `x86/multiboot.rs` 的内置 stub 是 Multiboot 风格，需要新增 Linux 专用 stub。

建议做法：

- 保留现有 `DEFAULT_BIOS_IMAGE` 供 ArceOS/NimbOS 使用。
- 新增 `DEFAULT_LINUX_BOOT_IMAGE`。
- 或者拆成：
  - `x86/multiboot.rs`
  - `x86/linux_boot.rs`

boot stub 职责：

- 从 real mode 启动。
- 设置基础段寄存器和栈。
- 准备 Linux boot protocol 需要的寄存器。
- 跳转到 Linux setup 或 protected-mode entry。

第一版可以走较保守的 real-mode Linux boot protocol；后续可考虑 PVH/direct long mode。

### 4. ImageLoader 自动识别 Linux 镜像

修改：

- `os/axvisor/src/vmm/images/mod.rs`

不建议为了 x86_64 Linux 新增 `boot_protocol` 之类的架构专用配置字段。为了让 x86_64 Linux VM 配置尽量和现有 aarch64/riscv64/Linux/ArceOS 客户机保持一致，启动协议应由 loader 根据镜像内容自动识别。

加载逻辑：

1. 在 x86_64 下，先尝试解析 kernel image 的 x86 Linux bzImage setup header。
2. 如果 header 校验通过，例如 `boot_flag == 0xaa55` 且 `header == "HdrS"`，则走 x86 Linux direct boot loader。
3. 如果不是 x86 Linux 镜像，则保持当前 x86 boot image + Multiboot info 路径，继续支持 ArceOS/NimbOS 等现有客户机。
4. `image_location = "memory"` 和 `image_location = "fs"` 都应走同样的识别逻辑；第一阶段建议优先使用 `memory`，与现有静态嵌入镜像流程保持一致。

这样不需要在 VM config 中新增 x86_64 专用参数。

### 5. x86 设备直通

客户机 Linux 的设备默认全部走直通，不需要为 Linux 专门实现模拟串口、模拟块设备或模拟 virtio 设备。Axvisor 在第一阶段应重点保证：

- VM config 能描述 x86 直通设备和直通地址范围。
- guest GPA 到 host HPA 的设备 MMIO/PIO 区域映射正确。
- 直通设备相关中断能投递到目标 vCPU。
- guest 启动元数据能描述或保留 Linux 识别设备所需的信息。

对于 QEMU x86_64 场景，推荐优先直通这些基础设备：

- 串口或 virtio-console，用于 early boot log。
- block 设备或 virtio-blk，用于后续 rootfs。
- Local APIC、IOAPIC、HPET/PIT/RTC 等 Linux 早期可能依赖的平台设备。
- PCI config space 和相关 BAR 区域，如果块设备/网卡等通过 PCI 暴露。

配置上应优先使用 `passthrough_devices` / `passthrough_addresses`，`emu_devices` 只保留给必要的半直通控制器或调试用途。

示例：

```toml
passthrough_devices = [
  ["COM1", 0x3f8, 0x3f8, 0x8, 0x1],
  ["IO APIC", 0xfec0_0000, 0xfec0_0000, 0x1000, 0x1],
  ["Local APIC", 0xfee0_0000, 0xfee0_0000, 0x1000, 0x1],
  ["HPET", 0xfed0_0000, 0xfed0_0000, 0x1000, 0x1],
]

passthrough_addresses = [
  [0x3f8, 0x8],
]

emu_devices = []
```

具体设备地址要以 QEMU/平台实际暴露的资源为准。

### 6. 中断和 timer 最小闭环

第一阶段不通过 VM 配置传递 `cmdline`。如果 Linux 需要 `console`、`rdinit`、`root` 等参数，应优先通过内核内建 command line、initramfs 默认行为或镜像构建流程解决，保持 VM 配置与其他客户机一致。

如果 Linux 仍依赖 timer/interrupt，需要补：

- 直通 PIT/HPET 或 LAPIC timer 的 MMIO/PIO 访问。
- 直通 PIC/IOAPIC/Local APIC 相关访问，或提供必要的半直通控制器。
- `os/axvisor/src/hal/arch/x86_64/mod.rs::inject_interrupt()` 当前为空，需要接到当前 vCPU 的 `inject_interrupt()`。
- `x86_vlapic` 已有 Local APIC 雏形，但如果目标是全直通 Linux 设备，应优先确认真实 LAPIC/IOAPIC/HPET 的 passthrough 与中断转发是否足够，而不是先实现完整模拟 LAPIC。

### 7. 后续 PCI / virtio 直通

进入 initramfs 后，如果要挂真实 rootfs，需要继续完善直通链路：

- PCI config space PIO: `0xcf8/0xcfc`
- virtio PCI transport 或 virtio-mmio 设备的直通映射
- 直通块设备对应的 BAR、queue、DMA 地址访问
- MSI/MSI-X 或 legacy INTx，中断注入到 guest
- IOAPIC / LAPIC 路由

这部分建议作为第二阶段，不阻塞第一阶段 initramfs 验证。方案原则是客户机 Linux 设备直通优先，不把新增模拟设备作为实现目标。
