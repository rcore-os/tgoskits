# axdevice

`axdevice` 是 Axvisor 的虚拟设备模型、设备图、资源规划与运行时管理 crate。它不解释客户机 TOML，也不规定各架构的初始化顺序。

## 核心边界

- `DeviceModel`：同一个 dyn 实例声明资源并构建 bundle；
- `DeviceGraphBuilder` / `ResolvedDeviceGraph`：保存稳定节点、依赖、host identity 和唯一资源计划；
- `ResourcePools` / `VmResourcePlan`：fixed-first、稳定 lowest-first 分配；
- `ResourceClaim` / `ResourceLease`：一次性消费和失败回滚；
- `DeviceBundle`：原子注册设备、controller、endpoint、service、grant 和 lifecycle；
- `DeviceRuntime`：封口后的 MMIO、PIO、SysReg 与中断能力索引；
- `FdtNodeModel` / `AcpiNodeModel`：从解析后资源生成设备固件片段。

crate 不再提供设备类型 enum、`DeviceFactoryRegistry`、裸配置直建设备或兼容 fallback。

## 模型阶段

```rust
pub trait DeviceModel: Send + Sync {
    fn declare(&self) -> DeviceManagerResult<DeviceDeclaration>;

    fn build(
        &self,
        context: &mut DeviceBuildContext<'_>,
    ) -> DeviceManagerResult<DeviceBundle>;
}
```

`declare()` 只产生命名 MMIO/PIO/IRQ/MSI slot。`build()` 只能从独占 `DeviceBuildContext` 消费这些 slot。图保存准确的 `Arc<dyn DeviceModel>`，不会在构建时按类型查找另一个 factory。

## 资源与中断

资源命名空间区分 MMIO、PIO、`(controller,input)`、host IRQ、ITS DeviceID/EventID 和 controller-global LPI。固定请求优先，自动请求按节点 ID 与 slot 排序并 lowest-first 分配。冲突错误包含 owner 与 requester。

`DeviceBuildContext::irq()` 从已注册的 `VirtualInterruptController` 获得 `WiredIrqInput`，创建当前设备独立的 `IrqLine`，并把 endpoint 与 lease 放入 bundle。edge、level 与 shared-level 的电气语义由 `axdevice_base` 保证。

## 固件

节点可挂载 `FirmwareModels { fdt, acpi }`。`render_device_firmware()` 按图顺序读取每个节点的 `ResolvedDeviceResources`，返回拥有所有权的片段。最终 FDT/ACPI 表间关系仍由架构 composer 负责。

## Runtime 与能力

runtime 通过 `handle_mmio_*`、`handle_port_*` 和 `handle_sys_reg_*` 分发访问。敏感操作使用 typed service、DMA/timer/wake/stop grant 和短生命周期 `DeviceAccess`；生产代码不 downcast `Arc<dyn Device>`。

controller bundle 必须先于依赖设备注册。bundle 任一步失败会恢复全部索引和资源 lease。全部图节点构建完成后 seal runtime，seal 后拒绝继续注册。

新增设备的完整步骤见 [Axvisor 虚拟设备模型与新增设备教程](/docs/architecture/axvisor/emulated-devices)。
