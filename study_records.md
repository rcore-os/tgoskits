# 学习记录

## 设备抽象重构当前判断

AxVisor 当前设备框架已经进入从 legacy 设备模型迁移到统一 `DeviceOps` 模型的阶段。核心目标不是简单把 `AxVmDevices::init` 里的 `match` 移到另一个文件，而是把设备模型拆成几层职责：

```text
设备本体：声明资源、能力、访问语义和生命周期
factory：根据 VM 配置创建设备实例
catalog：描述某个架构/平台可创建哪些设备
registry：保存已创建设备并负责总线路由和资源冲突检查
VM exit：只生成 BusAccess，不关心具体设备类型
```

当前已经形成的基础设施：

- `axdevice_base` 已承载基础模型：`DeviceOps`、`DeviceId`、`DeviceError`、`Resource`、`DeviceCapabilities`、`BusAccess`、`BusResponse`、`IrqSink` 等。
- `axdevice` 已有 `DeviceRegistry`，可以根据 `Resource::Mmio/Pio/SysReg` 建立路由，并做重复 `DeviceId` 和 bus resource 冲突检查。
- `LegacyDeviceAdapter` 已能把旧 `BaseMmioDeviceOps`、`BasePortDeviceOps`、`BaseSysRegDeviceOps` 适配成 `DeviceOps`。
- `AxVmDevices::add_*_dev` 当前仍保留旧 `Vec`，同时把 legacy 设备注册进 `DeviceRegistry`。
- 普通 MMIO/PIO/SysReg 访问入口已经可以收敛到 `DeviceRegistry::dispatch()`。

因此，后续新增 factory 时，不应该再把 factory API 设计成返回 `BaseDeviceOps`。`BaseDeviceOps` 是迁移期兼容层，不应该反向塑造新接口。

## 直接实现 DeviceOps 的原则

新的设备迁移原则是：**进入 factory 迁移范围的设备，默认直接由具体设备类型实现 `DeviceOps`**。

也就是说，目标形态应当是：

```text
EmulatedDeviceConfig
  -> DeviceFactory::build(ctx, config)
  -> Rc<dyn DeviceOps>
  -> AxVmDevices::register_device()
  -> DeviceRegistry
```

不推荐的形态是：

```text
EmulatedDeviceConfig
  -> DeviceFactory::build(...)
  -> BaseDeviceOps
  -> AxVmDevices 再包 LegacyDeviceAdapter
```

直接实现 `DeviceOps` 的好处：

- 设备自己的资源和能力声明留在设备代码里，而不是散落在 factory 或 wrapper 中。
- factory 只负责解析配置并调用设备构造函数，逻辑足够薄。
- 新增设备时主要修改设备 crate 和对应平台 catalog，不需要修改 `AxVmDevices::init`、`DeviceRegistry` 或总线分发核心。
- 可以逐步减少 `LegacyDeviceAdapter` 的使用范围，而不是让 legacy 过渡状态固化成新架构。

直接实现 `DeviceOps` 时，设备结构需要持有 registry metadata，例如：

```text
DeviceId
name
Vec<Resource>
DeviceCapabilities
```

为了减少重复样板，后续可以在 `axdevice_base` 中增加一个轻量 `DeviceMeta` helper。具体设备可以包含 `meta: DeviceMeta`，并在 `DeviceOps` 的 `id/name/resources/capabilities` 中直接转发。这个 helper 只是减少样板，不改变“设备本体直接实现 DeviceOps”的方向。

只有在少数场景下才考虑临时 wrapper：

- 设备结构来自外部 crate，短期不方便改字段。
- 设备内部状态生命周期和 registry metadata 确实不适合放在同一个结构里。
- 迁移过程中需要极短期桥接，且计划明确后续删除。

wrapper 不是默认路线。

## legacy 路径的定位

迁移过程中仍然需要保留 legacy 路径，但它的定位要清楚：

```text
LegacyDeviceAdapter：未迁移设备接入 DeviceRegistry 的兼容层
旧 MMIO/PIO/SysReg Vec：typed control path 和迁移期兼容层
```

它们不再是普通 bus 访问的目标路径。

旧 `Vec` 当前仍有实际用途：

- AArch64 可能通过遍历 MMIO 设备找到 `VGicD` 后执行 `assign_irq()`。
- RISC-V 可能通过查找 vPLIC 设备设置 pending。
- x86 当前还有 IOAPIC/PIT/serial 的专用句柄和控制路径。

这些行为不是普通 MMIO/PIO/SysReg 访问，而是架构控制路径。后续应逐步迁到更明确的 `InterruptRouter`、`IrqSink` 或 platform service，而不是强行用 bus dispatch 表达。

## catalog 的位置

仍然需要维护一个“可构造设备集合”，也就是 factory catalog。原因很简单：配置里只有设备类型和参数，系统必须知道哪个构造函数能处理它。

但 catalog 不应该放进 `axdevice` 核心并重新形成中心化大表。建议边界是：

```text
axdevice_base：
  定义 DeviceOps、Resource、BusAccess、DeviceFactory 等具体设备 crate 需要依赖的基础协议。

axdevice：
  定义 AxVmDevices、DeviceRegistry、注册流程、catalog 消费逻辑。
  不直接知道 VGicD、Gits、vPLIC、IOAPIC 等具体类型。

架构/平台 glue：
  组合该平台支持的 factory catalog，例如 AArch64 catalog 包含 VGIC/GPPT 相关 factory。

具体设备 crate：
  直接实现 DeviceOps，并导出自己的 factory 或 factory entry。
```

这样新增设备时，需要修改的是设备 crate 和平台 catalog，而不是修改核心 registry/router。

长期如果确实需要完全插件式自动发现，可以评估 linker-section 自动注册。但在 no_std/hypervisor 场景中，显式 catalog 更稳，链接脚本、LTO、多架构兼容和测试成本都更可控。

## 下一阶段路线

下一阶段建议围绕 AArch64 主线做 factory 和原生 `DeviceOps` 迁移：

```text
1. 定义 DeviceFactory / DeviceFactoryCatalog / DeviceBuildContext。
2. 让 factory 返回 Vec<Rc<dyn DeviceOps>>，不暴露 BaseDeviceOps。
3. 具体设备默认直接实现 DeviceOps；必要时补 DeviceMeta helper 减少样板。
4. 由 AArch64 平台 glue 组合 VGIC/GPPT factory catalog。
5. AxVmDevices::init 从“match 具体设备类型并构造”逐步变成“config -> catalog -> factory -> register_device”。
6. 保留 legacy fallback 服务未迁移设备，直到对应设备完成原生化。
```

AArch64 侧优先迁移这些当前 `AxVmDevices::init` 中的设备：

- `EmulatedDeviceType::GPPTDistributor` -> `arm_vgic::v3::vgicd::VGicD`
- `EmulatedDeviceType::GPPTRedistributor` -> 多个 `arm_vgic::v3::vgicr::VGicR`
- `EmulatedDeviceType::GPPTITS` -> `arm_vgic::v3::gits::Gits`
- `EmulatedDeviceType::InterruptController` -> `arm_vgic::Vgic`

其中 `GPPTRedistributor` 需要注意：一个 config 会根据 `cpu_num/stride/pcpu_id` 展开成多个设备实例，所以 factory 返回值必须支持多个 `Rc<dyn DeviceOps>`。

`IVCChannel` 暂时不要塞进普通 `DeviceOps` factory。它不是 bus device，而是 VM 级 meta resource/allocator。可以后续单独设计 VM resource initializer，避免污染设备 factory 模型。

## AArch64 VGicR 迁移验证记录

本阶段已经以 AArch64 主线的 `GPPTRedistributor -> VGicR` 作为第一条设备迁移样例，验证了 factory/native `DeviceOps` 路径是可行的。

实际落地后的路径是：

```text
EmulatedDeviceConfig(GPPTRedistributor)
  -> AArch64 DeviceFactoryCatalog
  -> GpptRedistributorFactory::build()
  -> VGicR::new(DeviceMeta, ...)
  -> Rc<dyn DeviceOps>
  -> AxVmDevices::register_device()
  -> DeviceRegistry
```

这次迁移中确认了几个重要原则：

- factory 直接返回 `Vec<Rc<dyn DeviceOps>>`，不再返回 `BaseDeviceOps`。
- `VGicR` 本体直接实现 `DeviceOps`，不再通过 wrapper 进入新模型。
- `VGicR` 删除了 `legacy_meta`、`new_with_meta` 和 `impl BaseDeviceOps<GuestPhysAddrRange>`。
- 原 `BaseDeviceOps` 中的 `address_range/read/write` 逻辑改为 `VGicR` 自己的私有方法，再由 `DeviceOps::access()` 调用。
- 设备 metadata 和资源声明由设备/factory 自己构造，核心容器只消费 `DeviceOps` 和 `Resource`。
- `AxVmDevices::init` 对已迁移设备只负责 catalog 查找、factory build、registry register，不再关心具体设备构造细节。

为了验证这条迁移链路，新增了一个专用测试组：

```text
test-suit/axvisor/aarch64-device-migration/qemu
```

对应命令：

```bash
cargo xtask axvisor test qemu   --arch aarch64   --test-group aarch64-device-migration   --test-case smoke   > tmp.log
```

当前验证配置只打开 `gppt-gicr`，暂时注释掉 `gppt-gicd` 和 `gppt-gits`，目的是避免 `VGicD` typed-control 路径和 `Gits` host ITS MMIO 初始化路径干扰 `VGicR` factory 迁移判断。

成功日志包括：

```text
aarch64 device factory matched: type=GPPTRedistributor
GPPT Redistributor factory built native VGicR for vCPU 0
aarch64 device factory built native device: id=DeviceId(0), name=gppt-gicr-0
aarch64 device factory registered 1 native device(s) for type GPPTRedistributor
PASS smoke
```

因此可以认为：**设备创建、资源声明、factory/catalog 接入、registry 注册这条迁移路径已经跑通**。

但这次验证不等于所有设备问题都解决了。日志中仍然可能出现：

```text
Failed to assign SPIs: No VGicD found in device list
```

这是 `VGicD` 仍被旧 typed-control 路径查找的结果，不属于 `VGicR` factory 迁移失败。它说明后续迁移 `VGicD` 时需要同时处理“设备控制面”问题，而不是只迁移普通 MMIO access。

## linker-section factory 自注册方案记录

在继续讨论“新增设备是否仍然要改 `device.rs`”时，确认了当前 `DeviceFactoryCatalog` 只解决了“构建设备逻辑不写在 `AxVmDevices::init` 里”的问题，还没有解决“factory 列表仍由核心代码维护”的问题。

当前 AArch64 路径中仍然存在类似下面的中心化注册点：

```text
device.rs
  -> 手写 factories: [&dyn DeviceFactory; N]
  -> DeviceFactoryCatalog::new(&factories)
  -> find(config.emu_type)
```

因此，新增一个 factory 仍然需要改 `device.rs` 或某个显式 platform catalog。这个状态比直接在 `match config.emu_type` 中构造设备更好，但还不是设备 crate 自注册。

项目里已经有一个可参考的 no_std linker-section 注册模式：`rdrive`/`ax-driver` 的 `.driver.register`。

现有模式大致是：

```text
driver crate
  -> #[link_section = ".driver.register"] static DriverRegister

runtime.ld
  -> __sdriver_register = .
  -> KEEP(*(.driver.register*))
  -> __edriver_register = .

axruntime/registers.rs
  -> start/end symbol
  -> &[DriverRegister]
  -> rdrive::register_append(...)
```

这个机制的关键不是 `inventory` 或 `linkme` 本身，而是更基础的 linker-section 思路：

```text
多个 crate 分散提交静态注册项
  -> 链接器把同名 section 合并
  -> 最终镜像通过 start/end 符号扫描注册项
  -> runtime/catalog 消费这些注册项
```

如果把它复用到 axdevice factory，建议做成一套平行机制，例如 `.axdevice.factory`。

建议的职责边界：

```text
axdevice_base：
  定义 DeviceFactoryRegister 或等价注册项类型。
  提供 register_device_factory! 宏。
  这样具体设备 crate 只依赖 axdevice_base，不依赖 axdevice，避免循环依赖。

具体设备 crate：
  定义自己的 factory。
  通过 register_device_factory! 把 factory entry 放进 .axdevice.factory section。

axdevice：
  读取 __saxdevice_factory/__eaxdevice_factory。
  将 linker section 中的注册项转换成 factory catalog。
  AxVmDevices::init 只消费 catalog，不再手写具体 factory 列表。

runtime/linker script：
  保留 .axdevice.factory* section。
  导出 start/end symbol。
```

一个可能的初始化路径：

```text
arm_vgic::v3::vgicr
  -> static GPPT_REDISTRIBUTOR_FACTORY
  -> register_device_factory!("gppt-redistributor", GPPT_REDISTRIBUTOR_FACTORY)

final ELF
  -> .axdevice.factory

axdevice
  -> linker_device_factories()
  -> DeviceFactoryCatalog
  -> factory.build(ctx, config)
  -> register_device()
```

需要特别注意依赖方向。注册宏如果放在 `axdevice`，`arm_vgic` 为了注册 factory 就要依赖 `axdevice`；但当前 `axdevice` 在 AArch64 下又依赖 `arm_vgic`，这会形成不合适的循环。因此注册项类型和宏更适合放在 `axdevice_base`。

no_std 下需要确认的正确性点：

- `runtime.ld` 或最终链接脚本必须 `KEEP(*(.axdevice.factory*))`，否则 LTO/`--gc-sections` 可能裁掉注册项。
- 需要导出 `__saxdevice_factory` 和 `__eaxdevice_factory`，并保证空 section 时 start/end 仍然可用。
- section 应放在已加载、可读、地址映射正确的区域，通常跟 `.runtime` 或 `.rodata` 附近更合理。
- section 起始地址要满足注册项类型的对齐要求，读取时要检查 section 字节长度是否是 `size_of::<DeviceFactoryRegister>()` 的整数倍。
- 注册项类型需要保持简单稳定，适合静态放入 section，例如名称、设备类型、factory 指针或构造函数指针。
- 如果使用 `&'static dyn DeviceFactory`，它只应作为同一个 Rust 镜像内的布局约定使用，不应把它当跨语言 ABI。
- 如果宏使用 `#[used(linker)]`，展开所在 crate 可能需要 `#![feature(used_with_arg)]`；如果为了降低接入成本，可以先使用稳定 `#[used]` 加 linker script `KEEP`。
- 设备 crate 必须被最终镜像实际链接进来。只写一个注册 static 不代表 Cargo 一定会把 crate 拉进最终 ELF。
- 不应依赖注册项顺序。若同一 `EmulatedDeviceType` 出现多个 factory，应定义清楚是报错、按 priority 选择，还是 first match。
- 构建后需要用 `readelf -S/-s` 或启动日志验证 section、start/end symbol 和 factory 数量。

和 `inventory`/`linkme` 相比，复用 `.driver.register` 思路更贴合当前项目：

- 不需要先引入新的分布式注册 crate。
- 可以沿用现有 linker script/start-end symbol/slice 扫描经验。
- 更容易在 Axvisor 的 no_std、多架构、定制链接脚本环境中审计。

这条路线能解决的是“新增 factory 不再改 `device.rs` 的 factory 数组”。它不能自动解决所有中心化修改：

- 新增全新的 `EmulatedDeviceType` 仍然需要改 `axvm-types` 和配置解析。
- 新设备 crate 仍然需要通过 feature/dependency 被最终镜像链接。
- 有 typed-control path 的设备仍然需要后续 IRQ/control-plane 抽象配合迁移。

因此，这个方案适合作为显式 catalog 之后的下一阶段增强：先把 factory 构建路径稳定下来，再把 factory 列表维护从核心代码迁移到 linker-section 注册表。

## 当前结论

设备迁移可以分成两层：

```text
设备创建和普通 bus access：
  当前方案已验证，可以继续按 VGicR 模板迁移。

设备控制面和中断注入：
  仍然散落在 VM/架构代码中，需要进入下一阶段设计。
```

因此后续不需要继续把主要精力放在“证明 factory 是否可行”上。后续普通设备迁移可以按需推进，但方向二的主线应该转向 IRQ 路由和中断后端抽象。

## 后续计划：转向 IRQ 路由迁移

下一阶段建议把重心放在 VM 级中断模型，而不是继续机械迁移所有设备。

目标形态：

```text
设备后端
  -> IrqSink / InterruptRouter
  -> arch-specific interrupt backend
  -> vLAPIC/vIOAPIC | VGIC | vPLIC/AIA | LoongArch virtual interrupt controller
```

核心原则：

- 设备只表达中断语义，例如 `raise/lower/pulse/msi/eoi`。
- 设备不直接调用 VGIC/vPLIC/vLAPIC/CSR/GCSR 等架构接口。
- 设备不通过“写另一个设备 MMIO”间接注入中断。
- VM 层提供统一 `InterruptRouter`，根据架构和 IRQ route 分发到实际后端。
- route 表负责描述 `IrqLine -> IrqTarget`、SPI/PPI/SGI、MSI/MSI-X、vCPU target 等关系。

建议拆成几个小步：

```text
1. 梳理现有中断注入路径
   AArch64: VGIC/GICH/ICH/LR 注入、VGicD::assign_irq、timer 中断
   RISC-V: vPLIC pending/claim/complete
   x86_64: vLAPIC/vIOAPIC/MSI/pending event
   LoongArch: CSR/GCSR 虚拟中断注入

2. 定义 VM 级 IRQ 概念层
   IrqLine
   IrqTarget
   IrqRoute
   IrqEvent
   IrqSink
   InterruptRouter
   InterruptControllerOps 或 ArchIrqBackend

3. 先做最小 AArch64 backend
   从设备视角只需要 pulse/raise/lower 一个 SPI/PPI。
   router 内部再调用现有 VGIC 注入逻辑。
   不急着一次性重写 VGICD/GICR/GITS。

4. 选择一个真实调用点迁移
   优先选择 timer tick、简单虚拟设备 IRQ、或当前最清晰的一条 SPI 注入路径。
   避免一开始就迁移完整 ITS/MSI。

5. 再回头处理 typed-control path
   VGicD::assign_irq 这类路径应从“遍历 MMIO 设备 downcast”迁到明确的控制接口或 interrupt backend service。
```

已有 `components/irq-framework` 更偏 host IRQ action/registry，可以借鉴它的 request/enable/dispatch 思想，但 VM 级 IRQ router 关注的是 guest interrupt injection，不应直接把两者混成一个对象。

短期里可以把后续工作拆成两条并行线：

- **设备线**：继续按 `VGicR` 模板迁移简单设备；遇到有控制面依赖的设备先记录 blocker。
- **IRQ 线**：优先设计和落地 VM 级 `InterruptRouter/IrqSink`，把架构特判从设备后端中抽出来。

我当前更倾向先推进 IRQ 线，因为设备创建路径已经被 `VGicR` 验证过，真正影响方向二收益的是“中断注入语义是否能统一”。
