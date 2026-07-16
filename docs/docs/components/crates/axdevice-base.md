# `axdevice_base`

> 路径：`virtualization/axdevice_base`

`axdevice_base` 是 `no_std` 的设备与中断能力契约层。它不解析配置、不创建设备、不
分配资源，也不拥有 VM 生命周期。

## 统一设备契约

`Device` 是新注册和热路径分发使用的主 trait：

- `name()`：稳定诊断名；
- `resources()`：构造时计算完成的 MMIO、PIO 或 sysreg ranges；
- `handle()`：处理一个类型化 `BusAccess`；
- `reset()`、`suspend()`、`resume()`：可选生命周期动作；
- `as_any()`：只用于确有必要的设备特有 data-plane 操作。

`DeviceRegistry` 负责构建期注册和冲突校验，`BusRouter` 负责 VM-exit 热路径 lookup 与
dispatch。`RegistryError` 明确区分结构错误、地址冲突、架构不支持和设备架构不匹配。

历史 `BaseDeviceOps<R>` 仍是轻量设备 core 的适配边界，可用：

- `MmioDeviceAdapter`；
- `PortDeviceAdapter`；
- `SysRegDeviceAdapter`。

`map_device_of_type()` 已弃用；controller capability 不应通过 downcast 获取，而应在
注册时单独提交。

## 中断强类型

中断契约不复用裸 `usize`：

- `InterruptControllerId`、`ControllerInputId`；
- `InterruptSourceId`、`IrqLineId`；
- `MsiDeviceId`、`MsiEventId`、`MsiMessage`；
- `InterruptEndpoint`、`InterruptTriggerMode`；
- `VcpuInterruptId` 和上层注册的 vCPU port/binding。

`WiredIrqInput` 由 controller capability 创建。每次 `connect()` 返回一个独立
`IrqLine` source；clone 保留同一 source identity。level input 在内部做 wired-OR，edge
input 只接受 pulse。`MsiEndpoint` 把 device/event message 发送到 controller-owned
`MessageInterruptSink`。

设备不能构造任意 endpoint，也不能凭 Guest vector 唤醒 vCPU。vCPU 关联属于
`InterruptTopology` 与 controller binding。

## 错误与并发

设备读写返回 `DeviceResult<T>`，中断动作返回 `IrqResult<T>`。错误携带 operation、
endpoint 和 backend detail，供 `axdevice` 或 AxVM 边界转换。

中断状态使用窄临界区完成计数或电平转换，再在锁外调用 sink。实现 sink 时同样不能在
持有 broad controller lock 的情况下回调设备或 vCPU。

## 验证

```bash
cargo test -p axdevice_base
cargo xtask clippy --package axdevice_base
RUSTDOCFLAGS="-D warnings" cargo doc -p axdevice_base --no-deps
```
