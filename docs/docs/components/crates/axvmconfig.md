# `axvmconfig`

> 路径：`virtualization/axvmconfig`

`axvmconfig` 是 Axvisor Guest TOML 的严格解析与 schema 层。它只保存用户请求，不探测
FDT/ACPI、不分配地址或 IRQ，也不会在解析后动态追加设备。平台发现和资源决策属于
`axvm::machine`。

## 配置边界

顶层配置由以下部分组成：

- `[machine]`：机型、固件种类和透传 IRQ 能力；
- `[base]`：VM identity、vCPU 数量与固定 pCPU 绑定；
- `[kernel]`：入口、镜像、ramdisk 和 boot firmware；
- `[[memory.regions]]`：显式 Guest 内存及 backing；
- `[devices]` / `[[devices.virtual]]`：deny、默认设备和虚拟设备请求。

关键表均使用 `deny_unknown_fields`。旧字段不会被忽略，而是返回可诊断的解析错误。

## MachineConfig

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

`interrupts_passthrough` 是透传机型的可选布尔能力，默认 `false`。即使写成 `false`，它
也不允许出现在 Virtual variant 中。解析结果通过 `MachineConfig::interrupt_delivery()`
立即转成：

- `InterruptDelivery::Mediated`：VM-local 控制器接收 host adapter 或虚拟设备信号；
- `InterruptDelivery::Direct`：只允许已经取得物理 IRQ ownership 的输入。

`firmware` 可为 `auto`、`fdt` 或 `acpi`。架构 adapter 会拒绝不支持的组合。

## MemoryConfig

```toml
[[memory.regions]]
guest_base = 0x8000_0000
size = 0x4000_0000
permissions = "rwx"
backing = { kind = "host", host_base = 0x8000_0000 }
```

`permissions` 只接受有序且不重复的 `r`、`w`、`x`，并且必须包含读权限。
`MemoryBackingConfig` variant 为：

- `allocate`：分配 VM-owned、清零内存；
- `identity-allocate`：用于 x86_64 或 AArch64 Passthrough VM，动态分配 VM-owned
  内存并令 GPA 等于 HPA，以支持无 IOMMU 的设备 DMA；配置中的 `guest_base` 必须为
  零占位符，并不表示 `[0, size)` 固定范围；
- `host`：显式映射并交接 host physical backing；
- `shared`：显式共享 backing，不取得设备 ownership；
- `reserved`：保留由平台策略拥有的 identity range。

所有固定 GPA 区域必须互不重叠；`identity-allocate` 的最终 GPA 由运行时分配结果决定，
因此不参与静态区间重叠判断，运行时映射仍会检查冲突并事务回滚。

## DevicesConfig

```toml
[devices]
disable_defaults = ["console"]
deny = [
  { kind = "fdt-path", value = "/soc/mmc@fe2b0000" },
  { kind = "interrupt", intid = 237 },
]

[[devices.virtual]]
id = "console0"
model = "arm-pl011"
source = { kind = "auto" }
backend = { kind = "host-console", rx = "exclusive", tx = "shared" }
```

`disable_defaults` 当前只接受 `console`。控制器、timer 和 power/reset 是 profile 的强制
基础设施，不能关闭。

`DeviceSelectorConfig` 支持：

| kind | 语义 |
| --- | --- |
| `fdt-path` | FDT 路径及其后代 |
| `acpi-path` | ACPI namespace 路径及其后代 |
| `compatible` | compatible、HID 或 CID |
| `mmio` | 与 `{ base, size }` 重叠的 host 设备 |
| `interrupt` | 指定 platform interrupt 的 owner |

`VirtualDeviceSourceConfig` 支持 `auto`、`allocate`、`fdt-path`、`acpi-path` 与
`compatible`。在 Virtual 机型中，`auto` 只做动态分配；显式 host selector 会被拒绝。
在 Passthrough 机型中，`auto` 按模型 predicate 和稳定 host 遍历顺序选择第一个未消费
模板，没有匹配时动态分配。

host-console backend 的 RX 可为 `exclusive` 或 `disabled`；TX 可为 `shared`、
`exclusive` 或 `disabled`。ownership 冲突由 AxVM 创建事务报告并回滚。

## 已删除的 API

这次配置变更是破坏性的，以下字段和对应 Rust 类型已删除且无兼容层：

- `base.vm_type`；
- `AddressSpacePolicy` 和独立 interrupt mode；
- `emu_devices`、`passthrough_devices`、`passthrough_addresses`、
  `passthrough_ports`、`excluded_devices`；
- `host_reserved_intids`；
- 裸设备地址、IRQ、类型编号与 `cfg_list`。

迁移时必须把物理设备选择改为 `deny`/默认授权，把模拟设备改为
`[[devices.virtual]]`，把内存改为 `[[memory.regions]]`。

## 完整示例

```toml
[machine]
mode = "passthrough"
firmware = "auto"
interrupts_passthrough = false

[base]
id = 1
name = "linux"
cpu_num = 2
phys_cpu_ids = [1, 2]

[kernel]
entry_point = 0x8008_0000
image_location = "fs"
kernel_path = "/guest/Image"
kernel_load_addr = 0x8008_0000
dtb_load_addr = 0x8800_0000

[[memory.regions]]
guest_base = 0x8000_0000
size = 0x4000_0000
permissions = "rwx"
backing = { kind = "allocate" }

[devices]
disable_defaults = []
deny = []

[[devices.virtual]]
id = "console0"
model = "arm-pl011"
source = { kind = "auto" }
backend = { kind = "host-console", rx = "exclusive", tx = "shared" }
```

## std 与验证

crate 默认保持 `no_std + alloc`；`std` feature 用于 TOML 文件工具、schema 和 host
fixture。仓库模板位于 `virtualization/axvmconfig/templates/`。

```bash
cargo test -p axvmconfig --no-default-features
cargo test -p axvmconfig --all-features
cargo xtask clippy --package axvmconfig
```
