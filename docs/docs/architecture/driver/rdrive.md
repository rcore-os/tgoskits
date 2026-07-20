---
sidebar_position: 3
sidebar_label: "设备管理"
---

# 设备管理

`rdrive` 是驱动框架的核心管理 crate。它负责驱动注册（`DriverRegister`）、设备探测（probe）分发、类型化设备 registry 和设备查询。`rdrive` 不包含具体硬件逻辑，也不依赖任何 OS runtime 或平台 HAL。

源码位于 `drivers/rdrive/src/`，入口 `lib.rs`。

## 核心 API

`rdrive` 的公共 API 围绕平台初始化、驱动注册和设备查询三组能力：

```rust
// 平台来源初始化
pub enum Platform {
    Static,
    Fdt { addr: NonNull<u8> },
    Acpi(probe::acpi::AcpiRoot),
}

pub fn init(platform: Platform) -> Result<(), DriverError>;
pub fn register_append(slice: DriverRegisterSlice);

// 探测
pub fn probe_pre_kernel() -> Result<(), ProbeError>;
pub fn probe_all(stop_if_fail: bool) -> Result<(), ProbeError>;

// 设备查询
pub fn get_device<T: DriverGeneric>(id: DeviceId) -> Result<Device<T>, GetDeviceError>;
pub fn get_one<T: DriverGeneric>() -> Option<Device<T>>;
pub fn get_list<T: DriverGeneric>() -> Vec<Device<T>>;
```

`Platform` 是初始化时传入的平台来源。仓库内置平台默认使用 FDT 或 ACPI；外部静态平台使用 `Platform::Static`。`init()` 只能调用一次，重复调用 panic。

## Manager

`Manager` 是 rdrive 的核心，源码位于 `manager.rs`。它只持有两个状态：

```rust
pub struct Manager {
    pub registers: RegisterContainer,
    pub(crate) dev_container: DeviceContainer,
}
```

- `registers`：所有已注册的 `DriverRegister`，按 `ProbeLevel` 和 `ProbePriority` 排序。
- `dev_container`：类型化设备 registry，`BTreeMap<DeviceId, DeviceOwner>`。

`Manager` 全局唯一，通过 `Once<Mutex<Manager>>` 保护：

```rust
static CONTAINER: Once<Mutex<Manager>> = Once::new();

pub(crate) fn container() -> &'static Mutex<Manager> { ... }
```

`Mutex` 使用 `ax-kspin::SpinRaw`，在多核环境下提供互斥访问。所有写操作（register、probe、insert device）经过 `edit()` 闭包，所有读操作（query）经过 `container().lock()`。

## DriverRegister

`DriverRegister` 描述一个驱动的注册信息，源码位于 `register/mod.rs`：

```rust
pub struct DriverRegister {
    pub name: &'static str,
    pub level: ProbeLevel,
    pub priority: ProbePriority,
    pub probe_kinds: &'static [ProbeKind],
}
```

| 字段 | 含义 |
| --- | --- |
| `name` | 驱动名，用于日志和诊断 |
| `level` | `PreKernel`（内核前早期 probe）或 `PostKernel`（普通 probe） |
| `priority` | 同 level 内的排序权重，值小者优先 |
| `probe_kinds` | 该驱动支持的平台来源和匹配规则 |

`ProbePriority` 预定义了关键平台设备的优先级：

```rust
pub const CLK: ProbePriority = ProbePriority(6);     // 时钟控制器
pub const INTC: ProbePriority = ProbePriority(10);   // 中断控制器
pub const DEFAULT: ProbePriority = ProbePriority(256);
```

时钟和中断控制器必须在其它设备之前 probe，因为后续设备的 clk 和 IRQ 解析依赖它们。

## ProbeKind 与 backend 分发

`ProbeKind` 是多来源分发的基础：

```rust
pub enum ProbeKind {
    Static { on_probe: static_::FnOnProbe },
    Fdt { compatibles: &'static [&'static str], on_probe: fdt::FnOnProbe },
    Acpi { ids: &'static [acpi::AcpiId], on_probe: acpi::FnOnProbe },
    Pci { on_probe: pci::FnOnProbe },
}
```

一个 `DriverRegister` 可以同时声明多个 `ProbeKind`，例如同一驱动既能从 FDT 发现，也能从 PCI 枚举。各 backend 的职责如下：

| backend | 独立状态 | 匹配输入 | probe 输入 | 职责 |
| --- | --- | --- | --- | --- |
| `probe::static_` | `System { probed_names }` | 显式注册的 driver name | `PlatformDevice` | 保留外部平台和板级 glue 的手工注册能力 |
| `probe::fdt` | `System { fdt, phandle_map, probed }` | compatible + node status | `FdtInfo` + `PlatformDevice` | FDT 设备树解析与匹配 |
| `probe::acpi` | `System { root, routing, pci, probed }` | HID/CID + ACPI device | `AcpiInfo` + `PlatformDevice` | ACPI source、MCFG/GSI routing、PCI `_PRT` |
| `probe::pci` | PCIe controller enumerator | vendor/device/class | endpoint + `PlatformDevice` | PCIe 二阶段 endpoint probe |

`probe_pre_kernel()` 只运行 `ProbeLevel::PreKernel`，通过 backend 分发器执行 Static、FDT、ACPI 中的早期 probe（interrupt controller、clock、timer、systick、pinmux、PCIe root complex）。PCI endpoint 枚举依赖已注册的 PCIe controller，因此在普通 probe 阶段触发。`probe_all(stop_if_fail)` 运行普通设备 probe，再执行 PCI endpoint 枚举。

## PlatformDevice 与设备注册

`PlatformDevice` 是 probe 回调中向 registry 注册设备的句柄，源码位于 `driver/mod.rs`：

```rust
pub struct PlatformDevice {
    pub descriptor: Descriptor,
}

impl PlatformDevice {
    pub fn register<T: DriverGeneric>(self, driver: T) { ... }
    pub fn register_pcie(self, drv: PcieController) { ... }
}
```

probe 回调构造硬件实例（实现某个 `rdif-*::Interface` trait），包装成 `DriverGeneric` 后调用 `register()`。`Descriptor` 携带 `DeviceId`、name、IRQ binding 等元数据。

## 类型化设备查询

`DeviceContainer` 提供 three 种查询方式：

| 方法 | 语义 | 典型调用方 |
| --- | --- | --- |
| `get_typed::<T>(id)` | 按 `DeviceId` 查询特定能力类型的设备 | 已知设备 ID 的低层 HAL |
| `get_one::<T>()` | 查询任意一个实现能力 `T` 的设备 | 单设备领域（如 primary display） |
| `devices::<T>()` | 查询所有实现能力 `T` 的设备 | 多设备领域（如多网卡、多块设备） |

返回的 `Device<T>` 是弱引用句柄，持有 `Arc<Mutex<T>>`。调用方 `lock()` 后获得 `&mut T`，可以调用 `rdif-*::Interface` 方法。

```rust
use rdrive::get_device;
use rdif_intc::Intc;

let intc: Device<Intc> = get_device(irq_id).expect("device not found");
let dev = intc.lock().unwrap();
dev.enable_irq(...);
```

直接使用 `rdrive::get_*` 只允许出现在设备管理型或低层 HAL 型代码中（例如 Starry USBFS host 管理、Axvisor AArch64 GIC backend）。普通 FS、NET、display、input、vsock 上层模块必须通过领域 service 消费设备，不得裸查 `rdrive`。

## 注册宏

`rdrive-macros` 提供 `module_driver!` / `model_register!` 宏，把 `DriverRegister` 放入 `.driver.register` linker section，启动时由 `register_append()` 统一收集：

```rust
#[unsafe(link_section = ".driver.register")]
#[unsafe(no_mangle)]
#[used(linker)]
pub static DRIVER: DriverRegister = DriverRegister { ... };
```

`ax-driver` 的 `model_register!` 宏基于同一机制，让具体驱动 crate 只需声明注册信息，不需要手动调用 `register_add()`。
