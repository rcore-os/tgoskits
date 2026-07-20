# `axvm`

> 路径：`virtualization/axvm`

`axvm` 是 VM 领域与资源生命周期层。它把不可变机型请求、host 平台快照、设备/中断
拓扑、Guest 地址空间、固件、vCPU 与启动状态组装成一个可事务创建的 `AxVM`。Axvisor
负责文件 I/O 和顶层编排；架构 crate 负责硬件细节。

## 统一机型流程

```text
VmMachineRequest
  + HostPlatformSnapshot
  -> VmMachinePlanner
  -> VmMachinePlan
  -> AxVM build transaction
```

`VmMachineRequest` 来自严格配置，创建后不可变。`HostPlatformSnapshot` 由 host FDT/ACPI
和可信 capability 构建，包含稳定设备 identity、资源和 ownership。planner 统一决定：

- Guest RAM、共享内存与 identity I/O mappings；
- host device 的 passthrough/deny/virtual replacement disposition；
- 虚拟设备 MMIO、PIO、IRQ 与 MSI 分配；
- 中断控制器布局和物理 IRQ forwarding policy；
- 最终 FDT/ACPI 需要描述的资源。

地址和 ID 分配使用 rust-vmm `vm-allocator`，Virtual FDT 使用 `vm-fdt`，ACPI/AML 使用
`acpi_tables`。host AML 不会被复制或裁剪到 Guest。

## 两种 Machine Mode

### Passthrough

Passthrough 从 host FDT/ACPI 得到设备模板。默认授权 `Assignable` 和 `Transferable`，
但强制保护、deny 和虚拟替换优先。
x86_64 不再用配置 feature 选择 VMX/SVM，而是在 host 初始化时按 CPUID 固定 backend，
并为所有参与虚拟化的 pCPU 校验一致性；nested page table 随 backend 选择 EPT 或 NPT。

非 RAM I/O aperture 默认 identity-map，同时对 host exclusive、reserved、deny、Guest
RAM、boot blobs、虚拟设备和虚拟 controller window 打洞。RAM 始终显式分配，未分配
host RAM 永不映射。

固定 GPA RAM 在 planning 阶段形成 I/O hole；`identity-allocate` RAM 则只记录大小，
由运行时 allocator 选择 host RAM 后令 GPA=HPA。配置中的零 `guest_base` 是占位符，
不能被当成低地址固定 RAM hole；FDT 在内存分配完成后使用实际范围重建。

已分配物理 IRQ 的 forwarding policy 有两种：

- `Mediated`：host IRQ adapter 连接 VM-local controller 的 software input；
- `HardwareForwarded`：取得 host IRQ ownership 后，使用固定 pCPU route 和 HW-backed LR。

该策略只作用于物理 source。PL011 等虚拟设备仍持有软件 `IrqLine`，并可与 HW-backed
物理 endpoint 共用一个 VM-local controller 和 CPU Interface。真正不经 EL2 的设备/CPU
bypass 属于独立的静态分区机型，不由该字段表达。

### Virtual

Virtual 不读取 host 设备模板，不映射 host MMIO/PIO/PCI。只映射显式 Guest RAM、共享
内存和 backing。虚拟设备窗口只注册到 bus，stage-2 保持 unmapped。controller 固定为
emulated，设备地址和 IRQ 从架构 profile 按稳定 instance ID 确定性分配。

## 创建事务

Axvisor 先读取 kernel、ramdisk 和外部 firmware，随后 AxVM 一次性 claim plan 中全部
physical device。`HostDeviceLease` RAII 保存交接状态；claim 竞争、snapshot generation
变化或后续失败会释放所有 lease。

构建顺序固定为：

1. RAM；
2. vCPU；
3. controller 与 vCPU binding；
4. devices 与 `InterruptTopology`；
5. bus 与 stage-2 mapping；
6. FDT/ACPI；
7. boot state；
8. commit。

controller、MMIO view 和设备 endpoint 作为同一 bundle/事务注册，避免半初始化 VM。

## 设备与中断边界

虚拟设备通过 `VirtualDeviceModel` 两阶段构建。第一阶段只声明具名资源需求，planner
完成分配后，第二阶段得到 `ResolvedDeviceResources` 和 `DeviceBuildContext`。

设备调用 `context.irq(slot)` 或 `context.msi(slot)` 获得 endpoint。设备实现不会看到
vCPU、controller ID、Guest INTID 或 host IRQ；设备到 controller input、controller 到
vCPU/上级 controller 的关系全部由 machine plan 与 topology 完成。

AArch64 PL011 与 Synopsys DW-APB UART 是完整示例：模型 core 只持有 level
`IrqLine`，AxVM adapter 提供 per-instance host-console backend、polling 和 FDT 节点。
DW-APB 与 packed NS16550 使用不同 model ID，运行时访问宽度、寄存器步长和固件属性
保持一致。CNTP timer 同样每 vCPU 持有 PPI line，并用 generation token 取消过期回调。

## 架构 profile

Virtual 标准 profile 的基础设施：

| 架构 | Controller | Timer/IPI | 默认 console | Firmware |
| --- | --- | --- | --- | --- |
| AArch64 | GICv3 | architected timer / PSCI | PL011 | FDT |
| RISC-V | PLIC | SBI timer/IPI/reset | NS16550 | FDT |
| x86_64 | LAPIC/IOAPIC | PIT | COM1 | ACPI |
| LoongArch64 | EIOINTC/PCH-PIC | timer/IPI | NS16550 | ACPI/fw_cfg |

console 可用 `disable_defaults = ["console"]` 关闭；controller、timer 和 reset 是强制
基础设施。Virtual AArch64 默认使用 PL011；host 派生的 Passthrough VM 会按固件选中
的 UART 使用 PL011、packed NS16550 或 DW-APB 虚拟替换。block、net、RNG 不会隐式
创建。

## AArch64 ownership

AArch64 从平台 capability 与 FDT 自动识别 host IPI/timer、GIC maintenance 与 Guest
EL1 timer role，不需要 TOML 列表。GICD/GICR 始终 trap；GICD SPI 按 endpoint ownership
过滤，host-owned 位 RAZ/WI，Guest GICR/SGI/PPI 状态完全保存在 VM 内，不修改 host
Redistributor。

可交接 SPI 只有在 host-side release 完成、planner claim 被消费后才能绑定物理 source；
失败或 Drop 会恢复 priority、trigger、route、pending/active 与 ownership。无隔离 ITS
capability 时不暴露物理 GITS，但虚拟 MSI 可使用独立的软件 ITS。

## 错误与 feature

library/domain 层返回可匹配的 `AxVmError`、`MachinePlanError`、`DeviceManagerError` 和
架构 backend 错误；Axvisor 边界再用 `anyhow` 添加文件和编排上下文。Guest 可触发路径
不使用 panic/todo 代替错误。

crate 默认保持 `no_std + alloc`；可选 `std` feature 用于领域测试和 host fixture。

## 验证

```bash
cargo test -p axvm --no-default-features --lib --tests
cargo test -p axvm --features std --lib --tests
cargo xtask clippy --package axvm
RUSTDOCFLAGS="-D warnings" cargo doc -p axvm --no-deps
```
