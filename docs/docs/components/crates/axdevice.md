# `axdevice`

> 路径：`virtualization/axdevice`

`axdevice` 是每 VM 的设备注册、总线分发、虚拟设备模型与中断拓扑层。它保持
`no_std + alloc`，可选 `std` feature 只用于 host 测试。资源选择、地址/IRQ 分配和固件
生成由 `axvm::machine` 完成；设备实现只消费已经解析的具名资源。

## 两阶段设备模型

旧 `DeviceFactory` 和按裸 `emu_type/base_gpa/irq_id/cfg_list` 创建对象的流程已删除。
`VirtualDeviceModel` 分成两个阶段：

1. `requirements(template)` 声明 MMIO、PIO、wired IRQ、MSI 与来源种类；
2. `build(resources, context)` 只使用 planner 分配的 `ResolvedDeviceResources` 创建设备。

```rust,ignore
impl VirtualDeviceModel for MyUartModel {
    fn model_id(&self) -> DeviceModelId { /* ... */ }

    fn requirements(
        &self,
        template: Option<&DeviceTemplate>,
    ) -> DeviceManagerResult<DeviceRequirements> {
        // Declare named slots; do not allocate addresses here.
        /* ... */
    }

    fn build(
        &self,
        resources: &ResolvedDeviceResources,
        context: &DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle> {
        let (base, size) = resources.mmio(&ResourceSlot::new("registers")?)?;
        let irq = context.irq(&ResourceSlot::new("irq")?)?;
        // The device owns `irq`; it never sees a vCPU or controller.
        /* ... */
    }
}
```

资源 slot 必须唯一且带语义，例如 `registers`、`irq`、`config`。模型可声明
`InterruptSourceKind::Software` 或 `Physical`，使 planner 在 direct interrupt delivery
下拒绝软件 IRQ。

## 中断拓扑

`InterruptTopology` 是每 VM 的控制器图。控制器注册可组合三种小 capability：

- `WiredInterruptInputs`：创建有线 controller input；
- `MessageInterruptInputs`：接收 MSI device/event；
- `VcpuInterruptController`：连接 `VcpuInterruptPort` 并返回 binding。

设备只通过 `DeviceBuildContext::irq(slot)` 获得 `IrqLine`，或通过 `msi(slot)` 获得
`MsiEndpoint`。这些 endpoint 只能由已注册控制器创建；共享 level 输入按 wired-OR
计数，同一连接的 clone 不会重复 source identity。

`finalize()` 在设备创建前后验证：

- 重复 controller ID 与默认 controller；
- 缺失 parent、输入范围和 trigger 冲突；
- controller cascade 环路；
- 重复 vCPU port；
- parent-first 注册和 binding 顺序。

controller output 可以连接上级 controller input。finalize 失败会断开已连接 cascade 并
丢弃 binding；VM 创建失败可调用 reset 后重新准备。公共 topology 不提供按 vector 的
`inject_irq`、裸 `set_level(line)` 或 `pulse(line)`。

## `IrqLine` 语义

`IrqLine` 代表一个设备连接，而不是全局 INTID：

- edge source 调用 `pulse()`；
- level source 在条件成立时 `raise()`，条件清除时 `lower()`；
- 多个 level source 共享 input 时，只有全部 source 都 lower 后 controller 才看到低电平；
- clone 同一个 `IrqLine` 共享 source identity；独立 `connect_irq` 得到独立 identity。

物理 IRQ adapter 持有 host ownership，并把 host line 的 electrical trigger 转换成同样
语义。Guest EOI/complete 后再 lower/unmask host level source，避免丢中断或中断风暴。

## 事务注册与总线

`DeviceBundle` 可以原子提交以下 capability：

- `DeviceRegistration::Device`；
- `DeviceRegistration::Pollable`；
- `DeviceRegistration::InterruptController`。

`AxVmDevices` 在提交时校验 MMIO、PIO 与 sysreg range，不允许地址重叠。任一 capability
失败会回滚同一 bundle 已注册部分。运行期通过统一 `Device`/`BusRouter` 路由 VM exit，
旧 `BaseDeviceOps` 设备可由 `MmioDeviceAdapter`、`PortDeviceAdapter` 或
`SysRegDeviceAdapter` 接入。

## 架构集成

- AArch64：GICv3 controller，PL011 与 timer 持有 `IrqLine`；
- RISC-V：PLIC source input 与 hart/vCPU context binding；
- x86_64：IOAPIC 到 LAPIC 的 message topology，COM1 使用每实例 backend；
- LoongArch64：PCH-PIC 到 EIOINTC 再到 vCPU。

架构 glue 可以封装平台仍需要的底层注入动作，但该动作不属于共享 public API，虚拟
设备也不能访问它。

## 错误与验证

`DeviceManagerError` 与 `IrqError` 是可匹配的领域错误，覆盖资源冲突、缺失 capability、
非法输入/trigger、backend failure 和 unsupported operation。Guest 可触发路径不以
panic 表示设备未命中。

```bash
cargo test -p axdevice --no-default-features
cargo test -p axdevice --features std
cargo xtask clippy --package axdevice
```
