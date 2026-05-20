# Axvisor x86_64 Linux 支持：第 6 阶段文档

本文档记录 `support_x86_64_linux_phases.md` 中第 6 阶段的实际推进结果。阶段 6 先完成了“与其他平台 Linux 客户机配置对齐，并让当前单 vCPU、no APIC bring-up 路径的 CPUID 暴露与实际能力一致”；随后开始从临时规避转向最小 vLAPIC / x2APIC 语义，为后续默认切换到 `linux-qemu` 做准备。

## 阶段目标

阶段 5 已经完成第一条可观察闭环：

- 默认 Ubuntu `bzImage` 能在 Axvisor 下启动。
- Linux 能解包 initramfs。
- Linux 能执行 `/init`。
- `/init` 能通过 `/dev/kmsg` 输出 marker：

```text
axvisor x86_64 linux initramfs reached /init
```

阶段 6 的当前目标是在不默认引入更多 x86 legacy 设备模型的前提下，把 x86_64 Linux VM 配置和 CPUID 暴露调整到与实际平台能力一致的状态：

- x86_64 Linux VM 配置与现有 aarch64 / riscv64 Linux 客户机的 `vm_type` 语义对齐。
- 单 vCPU 场景下先不向 Linux 暴露尚未支撑的完整 topology 和 TSC deadline timer 信息。
- 开始补齐 local APIC / x2APIC 的最小可运行语义，避免 Linux early APIC 路径踩到 `unimplemented!()`。
- 继续保留已验证能进入 `/init` 的默认 `bzImage`。
- 把 `linux-qemu` 的阻塞点记录为后续 APIC/timer/IRQ 平台能力补齐的输入，而不是当前阶段的默认内核切换条件。

## 本阶段已完成

### CPUID 暴露收敛和 APIC 最小暴露

`components/x86_vcpu/src/vmx/vcpu.rs` 的 VMX CPUID handler 做了阶段 6 第一轮收敛：

- 隐藏 nested VMX。
- 保留 hypervisor bit。
- 修正 MCE bit 清理位置，从 leaf 1 `EAX` 改为 leaf 1 `EDX`。
- 第一轮曾隐藏 local APIC / x2APIC，用于验证 `linux-qemu` 的 APIC/topology panic 是否来自能力暴露不匹配。
- 后续最小 vLAPIC 路线开始后，local APIC / x2APIC 已重新暴露。
- 继续隐藏 TSC deadline timer。
- 将 leaf 1 的 logical processor count 收敛为 1。
- 将 initial APIC ID 收敛为 0。
- 将 CPUID topology leaf `0xb` / `0x1f` 返回为空拓扑。

这组修改的目的不是完成完整 APIC 虚拟化，而是让当前“单 vCPU、最小 APIC bring-up”的 CPUID 结果逐步接近实际能力：可以让 Linux 进入 local APIC / x2APIC 早期路径，但仍不暴露多 CPU topology 和 TSC deadline timer。

### vLAPIC / x2APIC 最小语义

本轮开始补齐真正实现路径，而不是继续依赖 `noapic` / `nolapic` 规避：

- `components/x86_vcpu/src/vmx/vcpu.rs`
  - 拦截 `IA32_APIC_BASE` MSR 读写，并转发到 `x86_vlapic`。
  - x2APIC MSR 继续走 `EmulatedLocalApic`。
  - 启用 VMX APIC-access virtualization，设置 APIC-access page 和 virtual-APIC page。
  - 在 x86 VM 初始化时把 guest LAPIC GPA `0xfee0_0000..0xfee0_1000` 映射到 APIC-access backing page；否则开启 EPT 后硬件会先产生 LAPIC page 的 EPT violation，而不是进入 `APIC_ACCESS` VM exit。
  - `APIC_ACCESS` VM exit 接入 vLAPIC 的 32-bit MMIO read/write，覆盖 Linux 早期 `native_apic_mem_read()` 这类 xAPIC MMIO 访问。
- `components/x86_vlapic/src/lib.rs`
  - 暴露 `apic_base()` / `set_apic_base()`，供 VMX MSR handler 使用。
- `components/x86_vlapic/src/vlapic.rs`
  - 初始化 `IA32_APIC_BASE` 为 xAPIC enabled，BSP vCPU 带 BSP bit；x2APIC 由 guest 后续通过 MSR 写入显式开启。
  - 初始化 LAPIC ID 和 version，给 Linux 一个稳定的单 vCPU LAPIC 视图。
  - 修正 ICR destination mode 判断：`DestinationMode = 0` 才是 physical destination。
  - Self IPI 和 fixed IPI 会进入当前 VMM interrupt injection 链路。
  - EOI 不再触发 `unimplemented!()`；level-triggered EOI broadcast 仍记录为后续 IOAPIC 工作。
  - NMI、INIT、SIPI 在当前单 vCPU bring-up 中记录并忽略，避免早期 APIC 探测直接 panic。
- `os/axvisor/src/hal/impl_vmm.rs`
  - `active_vcpus()` 从 `todo!()` 改为根据 VM vCPU 数返回 active mask。
  - `inject_interrupt_to_cpus()` 遍历目标 vCPU mask，复用单 vCPU interrupt injection。

这仍不是完整 LAPIC/IOAPIC/PIC 实现，但已经把 Linux early APIC path 中最容易触发的硬 panic 点改为可继续推进的最小语义。

本轮验证曾发现一个新卡点：在恢复 local APIC / x2APIC 暴露、但尚未补齐 APIC-access EPT backing page 时，Ubuntu `bzImage` 会进入：

```text
x2apic: enabled by BIOS, switching to x2apic ops
NR_IRQS: 524544, nr_irqs: 32, preallocated irqs: 16
BUG: unable to handle page fault for address: ffffffffff5fd030
RIP: native_apic_mem_read
Call Trace:
  clear_local_APIC
  init_bsp_APIC
  init_ISA_irqs
  native_init_IRQ
```

这个结果说明 Linux 虽然选择了 x2APIC ops，但 BSP IRQ 初始化仍会通过 xAPIC MMIO fixmap 读取 LAPIC register。仅处理 x2APIC MSR 不够，必须让 LAPIC MMIO 访问也进入 vLAPIC。

随后补齐 APIC-access EPT backing page 和 `VIRT_APIC_ADDR` 后，该卡点已经解除。默认 x86_64 Linux 配置去掉 `noapic` / `nolapic` 后，60 秒 smoke test 能越过：

```text
APIC: Static calls initialized
APIC: Switch to virtual wire mode setup with no configuration
NR_IRQS: 524544, nr_irqs: 32, preallocated irqs: 16
```

并继续完成 PCI probing、virtio-blk driver probe 和 `vda` 识别。该结果说明阶段 6 的 LAPIC/APIC-access 早期 bring-up 已经形成最小闭环；新的阻塞面不再是 LAPIC ID/version MMIO 读取，而是后续 IOAPIC/IRQ/virtio-blk completion。

### linux-qemu 阶段性验证

`linux-qemu` 已确认是合法 x86 bzImage，裸 QEMU direct boot 能进入 `/init` 并输出 marker。

在 Axvisor 中，阶段 6 修改前 `linux-qemu` 会在 early boot 的 APIC/topology 路径 panic：

```text
native_apic_mem_read
parse_topology
```

修改 CPUID 暴露后，`linux-qemu` 已经不再触发该 panic，并能继续推进到：

```text
No local APIC present
APIC: disable apic facility
APIC: Switched APIC routing to: noop
Unpacking initramfs...
Freeing initrd memory: 12K
Serial: 8250/16550 driver, 4 ports, IRQ sharing enabled
```

当前仍未看到 `linux-qemu` 的 `/init` marker。它会停在 8250 串口驱动初始化附近，说明 APIC/topology panic 已解除，但后续还需要继续补 PIC/IOAPIC/timer/legacy IRQ 或串口相关路径。

### 串口接管排查

为了确认 `linux-qemu` 是否只是卡在 `console=ttyS0` 从 early console 切换到 8250 driver 的路径，本阶段临时把 command line 调整为：

```text
earlycon=uart8250,io,0x3f8,115200 keep_bootcon rdinit=/init acpi=off noapic nolapic pci=off i8042.nokbd i8042.noaux i8042.nomux
```

该配置去掉了普通 `console=ttyS0`，只保留 earlycon 和 `keep_bootcon`。验证结果仍然停在：

```text
Serial: 8250/16550 driver, 4 ports, IRQ sharing enabled
```

因此当前阻塞点不只是普通 console 接管问题，更可能与 legacy PIC/PIT/IRQ 状态或串口中断环境有关。

同时，x86 host platform 初始化会屏蔽 8259 PIC：

```rust
Port::<u8>::new(0x21).write(0xff);
Port::<u8>::new(0xA1).write(0xff);
```

而当前 VMX I/O bitmap 仍主要是直通策略，guest 对 legacy PIO 设备的访问会落到同一套 QEMU/host legacy 设备状态上。也就是说，在 `noapic` / `nolapic` 模式下，guest Linux 选择的 PIC/PIT legacy 路径并不是一个隔离的虚拟平台状态，而是在复用已经被 Axvisor host 初始化改写过的 PIC/串口/PIT 环境。

这个结论作为后续阶段输入保留。本阶段不默认加入隔离的 PIC/PIT/UART 模拟，也不在当前 VM 配置中注册额外 x86 legacy 设备。

### 与其他平台配置对齐

参考现有 aarch64 / riscv64 Linux 客户机配置，x86_64 Linux 的第一步支持继续保持 direct boot 和 passthrough bring-up 风格：

- `linux-x86_64-qemu-smp1.toml` 的 `vm_type` 与其他 Linux 客户机配置对齐为 `1`。
- 默认内核仍使用已验证能进入 `/init` 的 Ubuntu `bzImage`。
- `emu_devices` 保持为空，不在当前阶段默认注册额外的 x86 legacy PIC/PIT/UART 设备。
- 后续如果要支持 `linux-qemu` 或去掉 `noapic` / `nolapic`，再单独补 APIC、CPU topology、timer 和 IRQ 语义。

默认 Ubuntu `bzImage` 当前仍能进入 initramfs `/init`，并输出：

```text
Run /init as init process
axvisor x86_64 linux initramfs reached /init
```

使用临时配置把 `kernel_path` 切到 `linux-qemu` 后，`linux-qemu` 能越过早期 APIC/topology panic 和一部分平台初始化。早期结果曾停在：

```text
Serial: 8250/16550 driver, 4 ports, IRQ sharing enabled
```

APIC-access backing page 补齐后，默认 `bzImage` 已经能在 APIC enabled 路径下继续推进到 virtio-blk 设备发现。`linux-qemu` 是否能跟随推进仍需要单独复测；默认内核仍不应切换，直到 IOAPIC/IRQ/virtio-blk completion 闭合。

## 验证结果

已执行格式化：

```text
cargo fmt --all
```

已执行 x86_64 `x86_vcpu` 和 `axvm` 目标 clippy，检查通过：

```text
cargo clippy -p x86_vcpu --target x86_64-unknown-none -- -D warnings
cargo clippy -p axvm --target x86_64-unknown-none -- -D warnings
```

已执行 Axvisor x86_64 release build：

```text
cargo xtask axvisor build --config os/axvisor/configs/board/qemu-x86_64.toml --vmconfigs os/axvisor/configs/vms/linux-x86_64-qemu-smp1.toml
```

已使用默认 `linux-x86_64-qemu-smp1.toml` 做 APIC enabled smoke test。QEMU 日志能看到：

```text
APIC: Switch to virtual wire mode setup with no configuration
PCI: Probing PCI hardware
virtio_blk virtio0: [vda] 2097152 512-byte logical blocks (1.07 GB/1.00 GiB)
```

该 smoke test 最终由 `timeout` 结束；本轮没有看到 guest panic，也没有再出现 `GPA:0xfee00020` 的 LAPIC ID 读取 EPT fault。

后续阶段 7 加入最小 MP table 后，APIC/IOAPIC 平台发现还能进一步推进到：

```text
IOAPIC[0]: apic_id 1, version 17, address 0xfec00000, GSI 0-23
APIC: Switch to symmetric I/O mode setup
virtio-pci 0000:00:03.0: PCI->APIC IRQ transform: INT A -> IRQ 19
```

这说明阶段 6 的 LAPIC/APIC-access 工作已经能支撑后续 IOAPIC 路线；剩余阻塞点转入 timer / IOAPIC / virtio completion。

## 阶段结论

阶段 6 的第一步对齐已经完成，并开始进入最小 APIC 实现路线：

- x86_64 Linux VM 配置的 `vm_type` 已与其他平台 Linux 客户机对齐。
- VMX CPUID 暴露已收敛到当前单 vCPU、最小 APIC bring-up 的实际能力。
- vLAPIC 已具备 `IA32_APIC_BASE`、LAPIC ID/version、self/fixed IPI、EOI 和 active vCPU mask 等基础语义。
- 默认 Ubuntu `bzImage` 已能在 APIC enabled 路径下越过 LAPIC early bring-up，并继续到 PCI / virtio-blk 发现。
- `linux-qemu` 的默认切换仍等待 IOAPIC/IRQ/virtio-blk completion 闭合后复测。

后续要让 `linux-qemu` 成为默认 bring-up 内核，需要补齐至少一条隔离且一致的 x86 timer / interrupt 路径。可选方向包括：

- 继续推进当前 APIC 路径，补 MP table / ACPI MADT、IOAPIC、LAPIC timer、CPU topology 和中断投递语义。
- 或转向更隔离的设备模型路径，避免 guest 直接复用 Axvisor host 已经驱动的 QEMU PCI/virtio 设备。

这两类工作都属于更完整的 x86 平台模型支持，不纳入当前第一步对齐范围。
