# riscv-vplic

`riscv_vplic` 提供 RISC-V 虚拟 PLIC 设备实现。它是架构专用设备 crate，负责模拟 guest 可见的 PLIC 寄存器状态、pending/active IRQ 状态和 claim/complete 行为。

## 核心设备

`VPlicGlobal` 是 guest-visible PLIC MMIO 设备。它直接实现 `axdevice_base::Device`：

- `resources()` 声明 `Resource::MmioRange`；
- `access()` 处理 MMIO read/write；
- 内部 `read_register()` / `write_register()` 保留具体 PLIC 寄存器语义。

## 状态模型

`VPlicGlobal` 维护：

- source priority；
- pending bitmap；
- active bitmap；
- per-context enable mask；
- per-context threshold。

guest 的 claim/complete 操作只修改虚拟控制器状态，不直接破坏 host PLIC 的 IRQ 生命周期。

## Host mirroring

在 RISC-V host 上，部分路由相关配置需要镜像到 host PLIC，以保证物理中断源能到达 hypervisor。但 guest-visible pending、claim、complete 状态仍属于虚拟 PLIC。

## AxVM 接入

RISC-V 架构 bootstrap 创建 `VPlicGlobal`，注册对应 factory，并提供 `IrqSink` 让统一 `InterruptFabric` 可以把设备 IRQ 转换成 vPLIC pending 状态。

设备注册路径为：

```text
RISC-V bootstrap
        |
        v
VPlicGlobal + RiscvPlicFactory
        |
        v
DeviceBundle / DeviceRuntime
        |
        v
MMIO BusAccess -> Device::access()
```

## 当前状态

`riscv_vplic` 已完成 V3 化：

- `VPlicGlobal` 直接实现 `Device`；
- AxVM RISC-V factory 直接注册原生设备；
- 测试 mock 和 PLIC 测试已改为使用原生寄存器方法或统一 runtime 访问路径。
