# arm-vgic

`arm_vgic` 提供 AArch64 虚拟 GIC 和虚拟定时器设备实现。它是架构专用设备 crate，不放进 `axdevice`，但所有 guest-visible 设备都直接实现统一的 `axdevice_base::Device`。

## 主要设备

- `Vgic`：GICv2 风格虚拟中断控制器。
- `v3::vgicd::VGicD`：GICv3 distributor。
- `v3::vgicr::VGicR`：GICv3 redistributor。
- `v3::gits::Gits`：GICv3 ITS。
- `vtimer::SysCntpCtlEl0`：`CNTP_CTL_EL0`。
- `vtimer::SysCntpctEl0`：`CNTPCT_EL0`。
- `vtimer::SysCntpTvalEl0`：`CNTP_TVAL_EL0`。

## 接入方式

MMIO 设备声明 `Resource::MmioRange`，系统寄存器设备声明 `Resource::SysReg`。AxVM AArch64 factory 直接把这些设备作为 `Arc<dyn Device>` 放入 `DeviceBundle`。

```text
AxVM AArch64 factory
        |
        v
arm_vgic 具体设备
        |
        v
DeviceBundle / DeviceRuntime
        |
        v
BusAccess -> Device::access()
```

## vtimer

虚拟定时器由三个系统寄存器设备和一个共享 `VtimerState` 组成。设备本身只保存 guest-visible timer 状态，实际时间读取、timer 注册、取消和中断注入通过 `VtimerBackend` 完成。

这种分层使 timer 设备逻辑可以单测，同时避免系统寄存器设备直接依赖 AxVM runtime 内部结构。

## 中断控制器

GICv3 的 distributor、redistributor、ITS 保留原有寄存器语义和 host MMIO 后端行为，但设备框架入口已经统一为 `Device::access()`。

架构层如果需要与 GICD 协作，例如分配物理 IRQ，会通过 typed service 获取窄接口，而不是从设备 trait object 中取具体类型。

## 当前状态

`arm_vgic` 生产设备已经完成 V3 化：

- 设备本体直接实现 `Device`；
- 资源在设备构造时固定声明；
- AxVM 注册路径直接注册原生设备；
- vtimer 系统寄存器设备直接进入 SysReg bus dispatch。
