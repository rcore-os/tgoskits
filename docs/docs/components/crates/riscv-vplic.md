# riscv-vplic

`riscv_vplic` 提供 RISC-V 虚拟 PLIC 状态与寄存器语义。它负责 source priority、pending/active、per-context enable/threshold 以及 claim/complete；客户机操作不会直接破坏 host PLIC 生命周期。

## AxVM 接入

RISC-V 架构计划创建一个持有 `VPlicGlobal` 的 `Arc<dyn DeviceModel>`，把它作为控制器节点加入自己的设备图。model 声明 host-derived PLIC 固定 MMIO aperture，并在 `DeviceBundle` 中原子注册 guest-visible MMIO frontend 与 `Arc<dyn VirtualInterruptController>`。

普通设备依赖该 controller 节点，`DeviceBuildContext::irq()` 将计划好的 PLIC source 转换为独立 `IrqLine`。不存在通用 `InterruptFabric` 或第二份 pending 状态。

```text
RISC-V architecture plan
        |
        v
VPlic DeviceModel + controller registration
        |
        v
ResolvedDeviceGraph / DeviceBundle
        |
        v
DeviceRuntime -> MMIO access / wired input
```

RISC-V 仍独占 hart/context 初始化与 host mirroring 顺序；通用设备图只提供资源、claim、bundle 和回滚机制。
