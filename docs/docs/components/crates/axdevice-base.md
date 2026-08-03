# axdevice-base

`axdevice-base` 是 AxVisor 模拟设备框架的基础抽象 crate。它只定义跨 crate 共享的最小公共协议，不负责构建设备拓扑，也不保存 VM 的设备实例。

## 核心职责

- 定义统一设备 trait：`Device`。
- 定义一次设备访问的描述：`BusAccess`、`BusKind`、`BusResponse`。
- 定义设备可声明的静态资源：`Resource`。
- 定义设备访问期间的受限能力入口：`DeviceAccess`。
- 定义设备注册和总线分发错误：`RegistryError`、`DeviceError`。
- 定义中断线抽象：`IrqLine`、`IrqSink`。

## 统一设备模型

所有模拟设备都直接实现 `Device`：

```rust
pub trait Device: Send + Sync {
    fn name(&self) -> &str;
    fn resources(&self) -> &[Resource];
    fn access(
        &self,
        access: &BusAccess,
        context: &mut dyn DeviceAccess,
    ) -> Result<BusResponse, DeviceError>;
}
```

其中：

- `name()` 用于日志和诊断。
- `resources()` 在注册阶段声明设备占用的 MMIO、Port、SysReg 或 IRQ 资源。
- `access()` 是运行期热路径，处理一次具体 bus 访问。

## Resource

`Resource` 是设备拓扑的静态契约。设备注册时，runtime 会基于它做结构校验、地址冲突检测和架构适配检查。

当前支持：

- `MmioRange { base, size }`
- `PortRange { base, size }`
- `SysReg { addr, count }`
- `IrqLine { line, trigger }`

设备不应该在运行期临时声明资源；资源应在构造时固定下来。

## DeviceAccess

`DeviceAccess` 是一次访问期间的能力上下文。敏感能力默认不可用，设备必须持有注册时获得的 grant，且只能在当前 `access()` 调用中使用。

当前模型覆盖：

- guest memory DMA；
- timer；
- wake；
- stop。

这种设计让设备只拿到自己声明过、且当前访问需要的能力，避免设备对象长期持有过大的 VM 权限。

## 中断抽象

`IrqLine` 是设备侧看到的中断线句柄，内部绑定到 VM 的中断 fabric。设备不直接知道 GIC、PLIC、IOAPIC 等架构细节，只通过：

- `raise()`
- `lower()`
- `pulse()`

表达中断行为。

## 分层边界

`axdevice-base` 只放基础 trait、资源、错误和中断抽象。具体设备实现放在：

- `axdevice`：通用设备和可复用设备包；
- `arm_vgic`：AArch64 架构专用中断/定时器设备；
- `riscv_vplic`：RISC-V 架构专用 PLIC 设备；
- `axvm` 架构层：少量需要绑定 VM/架构上下文的设备工厂。

设备无论位于哪个 crate，都通过同一个 `Device` 模型接入运行期。
