# axdevice

`axdevice` 是 Axvisor 的虚拟设备模型、设备图、资源规划与运行时管理 crate。它不解释客户机 TOML，也不规定各架构的初始化顺序。

## 核心边界

- `DeviceModel`：同一个 dyn 实例声明资源并构建 bundle；
- `DeviceGraphBuilder` / `ResolvedDeviceGraph`：保存稳定节点、依赖、host identity 和唯一资源计划；
- `ResourcePools` / `VmResourcePlan`：fixed-first、稳定 lowest-first 分配；
- `ResourceClaimSet` / `ResourceLease`：按节点一次签发、按槽消费和失败回滚；
- `DeviceBundle`：原子注册设备、controller、endpoint、service、grant 和 lifecycle；
- `DeviceRuntime`：封口后的 MMIO、PIO、SysReg 与中断能力索引；
- `DeviceFirmwareSpec`：声明简单固件元数据和对应的资源槽。

crate 不再提供设备类型 enum、`DeviceFactoryRegistry`、裸配置直建设备或兼容 fallback。

## 模型阶段

```rust
pub trait DeviceModel: Send + Sync {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements>;

    fn firmware(&self) -> DeviceFirmwareSpec {
        DeviceFirmwareSpec::default()
    }

    fn build(
        &self,
        context: &mut DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle>;
}
```

`requirements()` 只产生命名 MMIO/PIO/IRQ/MSI slot。`firmware()` 声明节点名、compatible/HID、简单属性和使用哪些 slot。`build()` 只能从独占 `DeviceBuildContext` 消费这些 slot；接口直接接受 `mmio("registers")`、`irq("irq")` 等名称。图保存准确的 `Arc<dyn DeviceModel>`，不会在构建时按类型查找另一个 factory。

## 资源与中断

资源命名空间区分 MMIO、PIO、`(controller,input)`、host IRQ、ITS DeviceID/EventID 和 controller-global LPI。固定请求优先，自动请求按节点 ID 与 slot 排序并 lowest-first 分配。冲突错误包含 owner 与 requester。

`DeviceBuildContext::irq()` 从已注册的 `VirtualInterruptController` 获得 `WiredIrqInput`，创建当前设备独立的 `IrqLine`，并把 endpoint 与 lease 放入 bundle。edge、level 与 shared-level 的电气语义由 `axdevice_base` 保证。

## 固件

节点读取 model 的 `DeviceFirmwareSpec`，再与该节点的 `ResolvedDeviceResources` 组合。串口等常规设备使用通用元数据；GIC、ITS、PCI、MADT 和 `_PRT` 等架构拓扑由架构 composer 读取同一 resolved graph 生成，不存在第二组固件 dyn 容器。

## Runtime 与能力

runtime 通过 `handle_mmio_*`、`handle_port_*` 和 `handle_sys_reg_*` 分发访问。可选命中接口让 nested fault 和 x86 未映射 PIO 在一次索引后决定是否进入架构 fallback；运行路径没有 `find_*` 后二次 dispatch。敏感操作使用 typed service、DMA/timer/wake/stop grant 和短生命周期 `DeviceAccess`；生产代码不 downcast `Arc<dyn Device>`。

controller bundle 必须先于依赖设备注册。bundle 任一步失败会恢复全部索引和资源 lease。全部图节点构建完成后 seal runtime，seal 后拒绝继续注册。

新增设备的完整步骤见 [Axvisor 虚拟设备模型与新增设备教程](/docs/architecture/axvisor/emulated-devices)。
