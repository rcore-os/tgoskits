# Axvisor Guest FDT 与机型配置指南

Axvisor 不再从一组裸地址、IRQ 和设备类型编号拼接 Guest 设备树。VM 配置先转换成
不可变的 `VmMachineRequest`，再与 host 平台快照一起生成 `VmMachinePlan`；FDT 最终只
描述计划中已经授权并分配完成的资源。

## 机型

纯虚拟机型：

```toml
[machine]
mode = "virtual"
firmware = "auto"
```

透传机型：

```toml
[machine]
mode = "passthrough"
firmware = "auto"
interrupts_passthrough = false
```

`interrupts_passthrough` 只允许出现在 `mode = "passthrough"` 中，省略时为 `false`：

- `false`：host 捕获物理 IRQ，再通过 VM-local 控制器投递；虚拟设备可以持有软件
  `IrqLine`。
- `true`：物理 IRQ 直接投递，只能使用已经取得 host ownership 的物理中断源；带
  软件 IRQ 的虚拟设备会在规划阶段被拒绝。

解析后该字段会立即归一化为 `InterruptDelivery::{Mediated, Direct}`。控制器和设备
实现不接触配置布尔值。

旧 `vm_type`、`interrupt_mode`、`emu_devices`、`passthrough_devices`、
`passthrough_addresses`、`excluded_devices`、裸 `irq_id` 和 `cfg_list` 已删除，不提供
兼容解析。

## 内存

Guest RAM 必须显式声明。未分配的 host RAM 永远不会映射给 Guest。

```toml
[[memory.regions]]
guest_base = 0x8000_0000
size = 0x4000_0000
permissions = "rwx"
backing = { kind = "allocate" }
```

`backing.kind` 可为 `allocate`、`identity-allocate`、`host`、`shared` 或 `reserved`。
`host` 与 `shared` 需要提供 `host_base`。`identity-allocate` 用于 x86_64 或 AArch64
Passthrough VM：配置中的 `guest_base` 必须为零占位符，实际 GPA 取 VM-owned 分配的
HPA，以保留无 IOMMU 设备的 DMA 语义；零占位符不会形成 `[0, size)` 固定范围。固定
GPA 内存区域不得重叠。

## 设备策略

```toml
[devices]
disable_defaults = []
deny = [
  { kind = "fdt-path", value = "/soc/mmc@fe2b0000" },
]

[[devices.virtual]]
id = "console0"
model = "arm-pl011"
source = { kind = "auto" }
backend = { kind = "host-console", rx = "exclusive", tx = "shared" }
```

`disable_defaults` 当前只接受 `"console"`。timer、中断控制器和 power/reset 是架构基础
设施，不能通过设备配置关闭。

`deny` 支持以下稳定选择器：

- `fdt-path` 或 `acpi-path`：选择节点/对象及其后代；
- `compatible`：选择 compatible 或 ACPI hardware ID；
- `mmio`：按 `{ base, size }` 选择资源重叠设备并打洞；
- `interrupt`：按 `{ intid }` 选择中断所属设备。

## Passthrough FDT

AArch64 和 RISC-V 的透传 FDT 以 host FDT 与可信平台 capability 生成的
`HostPlatformSnapshot` 为输入。节点 ownership 分为：

- `HostExclusive`：host/hypervisor 永久占用；
- `Transferable`：创建 VM 时可事务性交接；
- `Assignable`：可直接分配；
- `Structural`：只保留总线、时钟或拓扑结构；
- `Unrepresentable`：无法安全隔离或无法生成合法 Guest 描述。

设备处理优先级固定为：强制保护、配置 deny、虚拟替换、剩余可分配设备透传。生成器
重新构造 phandle 引用，只保留最终 Guest 节点需要的依赖；不会把 host FDT 原样交给
Guest。

非 RAM 平台 I/O aperture 默认 identity-map，但 host 独占区、固件保留区、Guest RAM、
boot blob、deny 资源、虚拟设备窗口和虚拟控制器窗口始终打洞。虚拟 MMIO 在 stage-2
保持 unmapped，使访问触发设备模拟。

## Virtual FDT

Virtual 机型不读取 host 设备作为资源来源，也不映射 host MMIO/PIO/PCI。AArch64 默认
生成 GICv3、architected timer、PSCI 与 PL011；RISC-V 默认生成 PLIC、SBI 基础设施与
NS16550。地址、IRQ 与 phandle 从架构 profile 确定性分配，因此相同 instance ID 和配置
会得到稳定结果。

`source = { kind = "auto" }` 在 Virtual 机型中等价于动态分配，不会意外匹配 host
设备。

## AArch64 PL011

`arm-pl011` 模型声明一个 4 KiB MMIO 槽、一个 level-triggered SPI 和 24 MHz fixed
clock。设备构建时只获得具名资源以及 `DeviceBuildContext::irq("irq")` 返回的
`IrqLine`，不会看到 vCPU、控制器 ID、Guest INTID 或 host IRQ。

在 mediated Passthrough 机型中，`source = auto` 优先复用第一个未消费的 `arm,pl011`
模板的 Guest 地址、IRQ、clock 和固件属性，同时对真实 MMIO 打洞；没有匹配节点时再从
profile 资源池分配。Direct interrupt delivery 不允许虚拟 PL011。

生成的 FDT 包含 PL011、fixed-clock、`serial0` alias 和 `/chosen/stdout-path`。

## 创建与回滚

创建顺序固定为：RAM、vCPU、控制器/binding、设备/topology、bus/mapping、FDT/ACPI、
boot state、commit。Axvisor 先加载 kernel、ramdisk 和外部 firmware，随后一次性 claim
全部透传设备。claim 竞争、host snapshot generation 变化或后续任一步失败时，
`HostDeviceLease` 会恢复已经取得的设备和 IRQ ownership，整机创建不会留下半完成状态。

## 验证

配置可使用 `axvmconfig` 的 std 工具生成 schema 并做严格解析。常用本地验证命令：

```bash
cargo test -p axvmconfig --all-features
cargo test -p axvm --lib --tests
cargo xtask axvisor build -c os/axvisor/configs/board/qemu-aarch64.toml --debug
```
