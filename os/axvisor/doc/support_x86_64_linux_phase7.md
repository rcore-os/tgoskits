# Axvisor x86_64 Linux 支持：第 7 阶段文档

本文档记录第 7 阶段的当前推进结果。根据阶段 6 之后的边界，第 7 阶段先打通 PCI / virtio-blk 的设备发现路径，再继续推进 block I/O completion 和真实 rootfs 所需的读写能力。

## 阶段目标

阶段 7 的最终目标是让 x86_64 Linux 能发现并使用后续 rootfs 所需的 PCI / virtio / block 设备。当前阶段目标收敛为：

- 将已经验证的 PCI config / low MMIO 配置合并回默认 `linux-x86_64-qemu-smp1.toml`。
- 明确当前 x86 VMX PIO / MMIO / EPT 路径对 PCI 的实际行为。
- 让 Linux 能枚举 QEMU 暴露的 virtio-blk PCI 设备。
- 用 initramfs 中的只读 `/dev/vda` smoke test 验证 block I/O 是否真正可用。
- 如果 block I/O 仍卡住，将卡点收敛到 virtqueue DMA 或中断投递路径。

## 当前状态

### 默认配置已打开 legacy PCI config path

当前默认 x86_64 Linux command line 已从 `pci=off` 调整为：

```text
pci=conf1 pci=nomsi
```

这会让 Linux 使用 legacy PCI config mechanism #1，也就是 PIO `0xcf8/0xcfc` 路径；`pci=nomsi` 用来把当前验证面收敛到 legacy INTx，避免 MSI/MSI-X 在 IOAPIC/IRQ routing 支持补齐前引入额外变量。阶段 7 的临时 PCI 实验配置已经删除，相关配置合并回默认 `linux-x86_64-qemu-smp1.toml`。

当前默认配置已经不再使用 `noapic` / `nolapic`。阶段 6 补齐 LAPIC APIC-access backing page 后，Linux 能在 APIC enabled 路径下完成早期 LAPIC bring-up；当前阻塞点转移到 IOAPIC 平台发现和 virtio-blk completion。

### QEMU 已暴露 virtio-blk-pci

阶段 7 实验中曾把 `os/axvisor/configs/qemu/qemu-x86_64.toml` 的外层 QEMU rootfs 设备临时收敛为：

```text
virtio-blk-pci,drive=disk0,disable-modern=on,disable-legacy=off,vectors=0
```

这个形态适合验证 legacy virtio / INTx，但不适合作为默认外层 rootfs 设备：Axvisor host 侧自己的 block driver 可能无法识别 legacy-only 约束后的设备，导致 host 在文件系统初始化阶段 `No block device found` panic。

因此默认 QEMU 配置恢复为：

```text
virtio-blk-pci,drive=disk0
```

host QEMU 侧仍存在 PCI virtio block 设备；后续如果要继续做 legacy-only / INTx 实验，应使用单独的临时 QEMU 配置或新增第二块受控测试盘，避免破坏 Axvisor host rootfs 启动。默认配置已经能让 guest Linux 枚举并识别该设备；要真正挂载 rootfs，还需要继续验证：

- virtqueue DMA 使用的 GPA/HPA 映射关系正确。
- 设备中断能投递到目标 vCPU。
- guest 不能和 Axvisor host 同时把同一个外层 QEMU virtio-blk rootfs 设备当作自己独占的 virtio device 使用；当前更像 passthrough smoke，而不是完整设备所有权切换。

这些条件当前还没有形成完整闭环。

### x86 VMX PIO 当前主要是直通策略

`components/x86_vcpu/src/vmx/vcpu.rs` 中 VMX I/O bitmap 当前默认使用 passthrough 策略，初始化时只额外拦截 QEMU exit port：

```rust
let io_to_be_intercepted = QEMU_EXIT_PORT..QEMU_EXIT_PORT + 1;
```

因此 PCI config space 常用的 PIO 端口：

```text
0xcf8
0xcfc
```

当前不会被 Axvisor 作为 emulated PIO 设备处理，而是会落到外层 QEMU/host 的 PCI config 状态上。这个行为可以作为后续实验输入，但不能直接当成完整 PCI 虚拟化实现，因为 guest 看到的 PCI 状态、BAR 分配、DMA 和中断隔离都还没有一致模型。

### boot_params 已保留 passthrough MMIO range

x86 Linux boot_params 构造阶段会把 VM 配置中的 passthrough device range 和 passthrough address range 写入 E820 reserved：

```rust
for device in &self.config.devices.passthrough_devices {
    builder.add_reserved_range(...);
}
for address in &self.config.devices.passthrough_addresses {
    builder.add_reserved_range(...);
}
```

这能避免 Linux 把显式配置的 MMIO 区域当作普通 RAM 使用。阶段 7 已经把 QEMU q35 low MMIO window 加入默认配置：

```toml
passthrough_addresses = [
  [0xfe00_0000, 0x00c0_0000],
]
```

### MAP_IDENTICAL 与低端 boot scratch 已合并

为了让外层 QEMU virtio-blk 这类 DMA-capable passthrough 设备能按 guest 写入的地址访问内存，默认 x86_64 Linux 配置已把主 RAM 调整为 `MAP_IDENTICAL`：

```toml
memory_regions = [
  [0x0000_0000, 0x0800_0000, 0x7, 1],
  [0x0000_0000, 0x0010_0000, 0x7, 0],
]
```

其中第一段是 128 MiB identity-backed system RAM；第二段是低 1 MiB `MAP_ALLOC` boot scratch，用于 real-mode boot stub、`boot_params` 和 Linux real-mode trampoline。ImageLoader 在 x86 Linux direct boot + identity main memory 下会把 `kernel_load_gpa` 和 `ramdisk_load_gpa` 按实际 identity main memory base 做偏移，同时跳过通用 `config_guest_address()` 对 BSP entry 的 relocation，避免把低地址 boot stub 误判成需要随 kernel load address 一起搬移。

`boot_params` 的 E820 构造也已经从单 RAM range 扩展为多 RAM range：高地址 identity-backed system RAM 与低端 boot scratch 会同时暴露给 Linux，并在其中切出 `boot_params`、boot stub 和 VGA legacy hole 等 reserved 区间。这修复了 Linux 早期的：

```text
Kernel panic - not syncing: Real mode trampoline was not allocated
```

## 本阶段结论

阶段 7 已经打通 PCI config space、low MMIO window 和 virtio-blk 设备发现。PCI / virtio / block 仍不是单点开关，完整 rootfs 还需要继续验证：

- virtqueue DMA 地址路径。
- INTx / MSI / MSI-X 中断投递路径。
- guest IOAPIC/IRQ 平台描述，使 Linux 能配置 PCI legacy INTx 或后续 MSI/MSI-X。
- 外层 QEMU virtio-blk 设备的 ownership，避免 host 和 guest 同时驱动同一个设备。

当前默认配置能枚举到 `virtio_blk virtio0` 和 `vda`，但尚未重新到达 `/init` marker。这说明下一步应聚焦 virtio-blk 后续 I/O completion：如果 DMA 已经正常，主要风险会落在 x86 IOAPIC/IRQ routing、legacy INTx/MSI 路径，以及设备 ownership 上。

## 本阶段配置变更

阶段 7 曾临时新增 `linux-x86_64-qemu-smp1-pci.toml` 做受控实验。确认 PCI 枚举路径后，该文件已删除，配置合并回默认 `linux-x86_64-qemu-smp1.toml`：

- 去掉 `pci=off`。
- 增加 `pci=conf1 pci=nomsi`。
- 去掉 `noapic` / `nolapic`，默认走阶段 6 的最小 vLAPIC/APIC-access 路径。
- 增加 `0xfe00_0000..0xfec0_0000` low MMIO passthrough。
- 将主 RAM 调整为 `MAP_IDENTICAL`，并额外保留低 1 MiB `MAP_ALLOC` boot scratch。
- 外层 QEMU 默认 rootfs 设备保持普通 `virtio-blk-pci,drive=disk0`，legacy virtio / INTx 收敛只作为后续受控实验配置。

initramfs 构建脚本也加入了非破坏性 block smoke：`/init` 会尝试打开 `/dev/vda` 并读取 512 字节；成功时输出：

```text
axvisor x86_64 linux virtio-blk read /dev/vda ok
```

如果 `/dev/vda` 不存在或读取失败，则输出 skip marker，并继续 idle。

## 本阶段实验结果

### 第一次 PCI 实验

使用 `linux-x86_64-qemu-smp1-pci.toml` 运行后，Linux 已经进入 PCI 路径：

```text
PCI: Using configuration type 1 for base access
```

随后 Axvisor 反复报告：

```text
NestedPageFault { addr: GPA:0xfe000014, access_flags: WRITE }
```

这说明 `pci=conf1` 已经让 Linux 进入 legacy PCI config path，但第一个阻塞点不是 `0xcf8/0xcfc` PIO 本身，而是 QEMU q35 低 MMIO window 没有映射进 guest EPT。

### 映射低 MMIO window 后的结果

默认配置随后增加：

```toml
passthrough_addresses = [
  [0xfe00_0000, 0x00c0_0000],
]
```

该范围会被 Axvisor 以 GPA=HPA 方式映射进 EPT，并通过 boot_params 写入 E820 reserved。再次运行后，Linux 能完成 PCI 枚举并识别外层 QEMU 暴露的 virtio-blk-pci：

```text
PCI: Probing PCI hardware
pci 0000:00:03.0: [1af4:1001] type 00 class 0x010000 conventional PCI endpoint
pci 0000:00:03.0: BAR 0 [io  0xc000-0xc07f]
pci 0000:00:03.0: BAR 1 [mem 0xfebd5000-0xfebd5fff]
pci 0000:00:03.0: BAR 4 [mem 0xfe000000-0xfe003fff 64bit pref]
virtio_blk virtio0: 1/0/0 default/read/poll queues
virtio_blk virtio0: [vda] 2097152 512-byte logical blocks (1.07 GB/1.00 GiB)
```

这说明阶段 7 已经越过 PCI config space 和 virtio-blk 设备发现。合并到默认配置后再次运行，日志仍能看到 `virtio_blk virtio0` 和 `vda`，但 45 秒内未重新到达 `/init` marker，因此当前阻塞点进一步收敛为 virtio-blk 后续 I/O completion 或中断投递路径。

随后补齐了 x86_64 当前 vCPU interrupt injection 和 `inject_interrupt_to_cpus()` 的基础实现，并尝试为 QEMU virtio-blk legacy INTx 建立临时 IRQ bridge。debug 日志显示 vCPU 外部中断只有 host LAPIC timer vector `0xf0`，没有看到 virtio-blk 相关的 `0x23` 或 `0x2b`，也没有触发 IRQ forwarding。因此当前卡点不像是“host IRQ 已到但未回注”，更像是 passthrough 设备 DMA 没有完成。

进一步检查发现默认 x86_64 Linux 配置使用 `MAP_ALLOC` guest RAM，debug 日志中 GPA `0x0..0x0800_0000` 实际映射到 HPA `0x0960_0000..0x1160_0000`。但外层 QEMU 的 virtio-blk-pci 是物理 passthrough 设备，它按 guest 写入的 DMA/GPA 地址访问 host 物理地址，不能理解 Axvisor 的 EPT 分配映射。这解释了为什么 Linux 能枚举到 `vda`，但 block request completion 迟迟无法出现。

随后默认配置切换到 `MAP_IDENTICAL` 主 RAM，并补齐了 x86 Linux direct boot 的特殊布局处理：通用 guest address relocation 不再作用于低地址 boot stub；ImageLoader 会把 protected-mode kernel 和 initramfs 放入实际 identity main memory；低端 `MAP_ALLOC` boot scratch 会通过 E820 作为可用低端内存暴露给 Linux。再次运行后，Linux 已能看到低端可用 E820 range、高地址 identity RAM，并越过 real-mode trampoline panic，继续枚举 PCI 和 `virtio_blk virtio0: [vda]`。

本轮又把外层 QEMU virtio-blk 约束为 legacy 设备，并在 guest cmdline 增加 `pci=nomsi`。`axvmconfig check` 已确认默认 VM 配置可解析。`cargo axvisor qemu` 这轮被另一个 cargo metadata 进程持有 package cache lock，60 秒内未进入 QEMU；直接复用现有 release 镜像运行 QEMU 可以进入 Axvisor，但该镜像是 debug 日志级别，VMX bind/unbind trace 淹没了 Linux 输出，因此需要重新构建 info 级别 release 镜像后再做下一轮 marker 验证。

### info release smoke 与 x86 IRQ 前向实验

重新构建 info release 镜像后，使用默认 `linux-x86_64-qemu-smp1.toml` 运行 90 秒 smoke test，Linux 仍能稳定完成：

```text
PCI: Using configuration type 1 for base access
PCI: Probing PCI hardware
virtio-pci 0000:00:03.0: virtio_pci: leaving for legacy driver
virtio_blk virtio0: 1/0/0 default/read/poll queues
virtio_blk virtio0: [vda] 2097152 512-byte logical blocks
```

本轮还加入了一个收敛的 x86 passthrough IRQ 前向实验：当 VM exit 收到 host IOAPIC 普通外设 vector，并且 VM 配置为 `interrupt_mode = "passthrough"` 时，将该 vector 作为 external interrupt 排入当前 guest vCPU。该实验不替代虚拟 PIC/IOAPIC，只用于判断当前卡点是否是“host IRQ 已到但未回注 guest”。

90 秒 smoke test 中没有出现 IRQ forward marker，也没有到达 `/init` marker 或：

```text
axvisor x86_64 linux virtio-blk read /dev/vda ok
```

这说明当前阶段已经完成 PCI config、BAR/low MMIO、legacy virtio-blk 发现和 identity DMA 布局验证，但 block I/O completion 仍未闭合。

### APIC enabled 后的最新 smoke

阶段 6 补齐 APIC-access backing page 和 `VIRT_APIC_ADDR` 后，默认配置移除了 `noapic` / `nolapic`。第一轮 60 秒 smoke test 不再出现 LAPIC ID 读取 EPT fault，也没有 guest panic。Linux 能继续到：

```text
APIC: ACPI MADT or MP tables are not detected
APIC: Switch to virtual wire mode setup with no configuration
Not enabling interrupt remapping due to skipped IO-APIC setup
PCI: Probing PCI hardware
virtio_blk virtio0: [vda] 2097152 512-byte logical blocks (1.07 GB/1.00 GiB)
```

这说明当前 APIC 路线已经越过 LAPIC early bring-up，但 guest 仍没有可发现的 IOAPIC 平台描述。Linux 因此跳过 IOAPIC setup，PCI legacy INTx 仍没有一致的 guest interrupt routing。

随后加入了最小 Intel MP table：

- 低端 `0x9f800..0xa0000` 写入 MP config table 和 `_MP_` floating pointer。
- E820 把该区域标记为 reserved。
- MP table 描述 1 个 BSP LAPIC、1 个 IOAPIC、ISA bus、PCI bus，以及保守的 ISA IRQ / PCI INTx source override。

修正 MP entry type 后，Linux 已能识别：

```text
found SMP MP-table at [mem 0x0009fc00-0x0009fc0f]
Intel MultiProcessor Specification v1.4
MPTABLE: OEM ID: AXVISOR
MPTABLE: Product ID: X86LINUX
IOAPIC[0]: apic_id 1, version 17, address 0xfec00000, GSI 0-23
APIC: Switch to symmetric I/O mode setup
virtio-pci 0000:00:03.0: PCI->APIC IRQ transform: INT A -> IRQ 19
```

不加额外参数时，Linux 会在 IOAPIC timer 自检处 panic：

```text
Kernel panic - not syncing: IO-APIC + timer doesn't work!
```

这是预期的新卡点：MP table 让 Linux 真正进入 IOAPIC 路径，但 Axvisor 还没有虚拟 PIT/IOAPIC timer 或等价 timer interrupt 模型。为了继续观察 PCI/virtio，默认 cmdline 临时加入：

```text
nox2apic no_timer_check
```

其中 `nox2apic` 避免缺少 IRQ remapping 时的 x2APIC 路径变量，`no_timer_check` 只跳过早期 IOAPIC timer 自检，不代表 timer 已完成实现。带该参数后，Linux 可以越过 timer panic，完成 IOAPIC/PCI INTx route 建立、PCI probing、virtio-blk driver probe 和 `vda` 识别，但 60 秒内仍未到达 `/init` marker 或 block read marker。

这说明下一步不应继续围绕 PCI 枚举参数反复试探，而应补齐 x86 guest interrupt controller / platform description：

- 短线继续补虚拟 PIT/IOAPIC timer 或等价 timer interrupt，使 `no_timer_check` 可以移除。
- 中线实现虚拟 IOAPIC/PIC 或更完整的 IRQ routing，而不是把 host IOAPIC 事件粗暴回注到 guest。
- 设备侧应避免长期依赖外层 QEMU host rootfs virtio-blk 被 guest 同时驱动；更干净的路线是单独测试盘、正式 passthrough ownership，或实现 emulated virtio-blk。

按总阶段文档的验收边界评估，第 7 阶段当前完成度约为 65%-70%：

- 已完成默认配置合并、PCI config path、low MMIO passthrough、virtio-blk legacy device discovery、`vda` 识别、MAP_IDENTICAL DMA 布局和低端 boot scratch/E820 修复。
- 未完成 initramfs block read marker、`/init` 后续闭环、legacy INTx/PIC 或 APIC 路由的正式实现。

## 后续任务

1. 定位 Linux 识别 `vda` 后未进入 `/init` 的具体等待点。
2. 检查 x86_64 interrupt injection 链路：
   - `components/x86_vcpu` 的 VMX vCPU 已有 `queue_event()` / `inject_interrupt()` 能力。
   - aarch64/riscv64/loongarch64 的 HAL `inject_interrupt()` 均已有平台实现，分别落到 GIC virtual interrupt、vPLIC pending bit 和 LoongArch guest interrupt controller/status。
   - `os/axvisor/src/hal/arch/x86_64/mod.rs::inject_interrupt()` 已补齐当前 VM/当前 vCPU 的转发层，会通过 `axvisor_api::vmm::inject_interrupt()` 进入 VMX vCPU 的事件队列。
   - `inject_interrupt_to_cpus()` 已从 `todo!()` 改为遍历目标 vCPU mask 并复用单 vCPU 注入接口。该实现能完成目标 vCPU 的事件排队，但远端 pCPU 唤醒/异步 IPI 投递仍依赖 `with_vm_and_vcpu_on_pcpu()` 后续补齐。
3. 补齐 x86 guest interrupt controller / platform description 路线：
   - 最小 MP table 已能让 guest 发现 LAPIC + IOAPIC，并把 virtio-blk `INT A` 转成 IRQ 19。
   - 短线补虚拟 PIT/IOAPIC timer，使 guest 不再依赖 `no_timer_check`。
   - 中线实现虚拟 IOAPIC/PIC 或正式 IRQ routing，为 PCI legacy INTx、MSI/MSI-X 和多 vCPU 做准备。
4. 为 x86 passthrough DMA 设计正式方案：
   - 支持固定 GPA=HPA 的低端 guest RAM 映射，并避免触发当前内核 relocation 逻辑；或
   - 通过 IOMMU/virtio emulation/地址转换层让外部设备能访问 `MAP_ALLOC` 后的 HPA。
5. 待 block 设备可读写后，再进入阶段 8 的真实 rootfs 启动。

## 验收边界

本阶段当前验收不是“Linux 已经能挂载真实 rootfs”，而是：

- 默认配置已承接 PCI/virtio-blk 设备发现路径，不再依赖临时 PCI 配置。
- PCI/virtio/block 的当前阻塞面被拆清楚。
- 后续工作聚焦 IOAPIC/IRQ routing、virtio-blk I/O completion 和设备 ownership，而不是 PCI config 枚举。
