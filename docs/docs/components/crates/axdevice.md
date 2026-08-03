# axdevice

`axdevice` 是 AxVisor 的设备聚合与运行期管理 crate。它负责把静态设备配置和架构 bootstrap 贡献的设备 bundle 统一注册进 `DeviceRuntime`，并在运行期把 MMIO、Port I/O、SysReg 访问分发给对应设备。

## 核心职责

- 提供 `DeviceRuntime`。
- 提供 `DeviceFactory` / `DeviceFactoryRegistry`。
- 提供 `DeviceBundle` 和设备生命周期注册。
- 提供 typed service registry。
- 提供内置通用设备，例如 `fw_cfg`、LoongArch PCH-PIC、x86 IOAPIC/PIT/串口等。

## 设备注册路径

普通设备通过 factory 构建：

1. VM 配置给出 `EmulatedDeviceConfig`。
2. `DeviceFactoryRegistry` 根据设备类型找到 factory。
3. factory 返回 `DeviceBundle`。
4. `DeviceRuntime` 事务式注册 bundle 中的设备、service、poller、lifecycle。
5. runtime 校验资源冲突并建立 bus dispatch 索引。

架构 bootstrap 也使用同一套 bundle/factory 思路，只是输入可能来自架构探测或 boot payload，而不是普通配置项。

## 运行期分发

`DeviceRuntime` 对外提供：

- `handle_mmio_read/write`
- `handle_port_read/write`
- `handle_sys_reg_read/write`

这些入口都会转换成统一的 `BusAccess`，再查找匹配 `Resource` 的设备并调用 `Device::access()`。

设备返回：

- `BusResponse::Read { value }`
- `BusResponse::Write`

runtime 会校验读写方向，避免读请求返回写响应、写请求返回读响应。

## Bundle

`DeviceBundle` 是一个原子注册单元。它可以同时携带：

- guest-visible `Device`；
- typed service；
- pollable device；
- lifecycle hook；
- DMA/timer/wake/stop grant 绑定。

注册 bundle 时，任意一步失败都会回滚之前已注册的资源，避免半注册状态。

## Service

service 用于设备和架构层之间的窄接口协作，例如：

- LoongArch PCH-PIC 输出端口；
- x86 IOAPIC/PIT 服务；
- vtimer backend 服务。

生产路径不通过 downcast 从 `Arc<dyn Device>` 里取具体设备，而是通过明确的 service key 获取需要的能力。

## 能力受限模型

需要敏感能力的设备必须在注册时绑定 grant。运行期每次访问都会创建短生命周期 `DeviceAccess`，设备只有在持有正确 grant 时才能使用对应能力。

这保证：

- 普通设备无法访问 guest memory；
- DMA 能力不会变成长期全局句柄；
- timer/wake/stop 等能力有明确授权边界。

## 当前状态

当前生产设备路径已经统一到 V3 `Device` 模型。通用设备、架构专用中断控制器、系统寄存器设备和测试 mock 均直接实现 `Device`，不再需要旧兼容接入层。
