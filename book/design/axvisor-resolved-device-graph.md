# Axvisor 解析后设备图与客户机固件

状态：PR #1718 的实现基线

## 问题与目标

旧实现分别在客户机配置、machine 默认值、设备类型枚举、factory registry、总线注册、FDT/ACPI 生成和架构启动代码中描述同一设备。新增设备既要修改中心枚举和多处 `match`，又要人工协调地址与中断号；同一个数字还可能在一层通过校验、在另一层发生冲突。

本设计把每个设备的类型化模型和解析后的设备图设为唯一事实来源：

```text
Guest TOML: id + model + options
              |
ConfiguredDeviceCatalog（显式 registration）
              |
       Arc<dyn DeviceModel>
              |
架构 DeviceGraphBuilder + host firmware snapshot
              |
       确定性资源规划
              |
       ResolvedDeviceGraph
         /             \
FDT / ACPI 片段       DeviceRuntime
```

普通虚拟设备只声明需要几段 MMIO/PIO、几个有线中断或 MSI，不填写数字资源。平台设备和 host replacement 可以使用来自 machine profile 或 host 固件的固定资源。固件模型与运行时构建消费同一份 `ResolvedDeviceResources`。

通用层只统一机制，不统一四种架构的初始化策略。AArch64、RISC-V、x86 和 LoongArch 仍分别控制控制器创建、vCPU 绑定、地址空间和固件固化顺序。

## 分层与所有权

- `axdevice_base`：类型化中断 ID、电气中断线语义和最小控制器能力。
- `axdevice`：dyn 设备模型、设备图、资源规划、claim/lease、bundle、runtime 索引和设备固件片段。
- `axvmconfig`：开放式 `id + model + options` 用户配置，不理解具体设备选项。
- `axvm`：配置 factory catalog、各架构计划、host 固件快照、FDT/ACPI 合成、地址空间、vCPU 和架构设备顺序。

具体中断控制器始终是 enable、pending、active、route、EOI 和硬件 backing 的唯一所有者。设备图只保存拓扑和资源事实；`DeviceRuntime` 只保存路由索引与能力句柄，不建立第二份中断状态。

## 开放式配置边界

用户可写：

```toml
[[devices.virtual]]
id = "data0"
model = "virtio-blk-mmio"
capacity = "20GiB"
backend = { type = "file", path = "/images/data.raw" }
```

`VirtualDeviceRequest` 只保留稳定 ID、规范 model 名和剩余 TOML table。它禁止用户填写 MMIO、PIO、IRQ、MSI 或 LPI 数字。catalog 中的 `ConfiguredModelRegistration` 保存规范 model 名和普通构造函数指针；构造函数使用带 `deny_unknown_fields` 的类型化结构解析 options，并直接返回 `DeviceNodeSpec`。未知 model、重复注册、重复设备 ID 和未知选项都明确失败，不存在额外 factory trait 或 instance 包装层。

catalog 由代码显式构造；不使用 linker section、全局静态发现、动态库或外部插件。新增标准设备只增加自己的模块并在标准 catalog 注册一次，不修改设备类型枚举或四个架构的中心 `match`。本 PR 不注册真正的 virtio-blk，因此上例会返回 `UnknownVirtualDeviceModel`，不会伪装成功。

`DeviceInstantiationContext` 只暴露架构和默认 wired/MSI 域等小型能力，不暴露架构内部对象、裸 IRQ 或设备管理器。

## dyn 设备模型

图节点保存声明和构建都使用的同一个 `Arc<dyn DeviceModel>`：

```rust
pub trait DeviceModel: Send + Sync {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements>;

    fn firmware(&self) -> DeviceFirmwareSpec {
        DeviceFirmwareSpec::default()
    }

    fn build(
        &self,
        context: &mut DeviceBuildContext<'_>,
    ) -> Result<DeviceBundle, DeviceManagerError>;
}
```

`requirements()` 是纯声明阶段，只能返回命名资源槽和能力需求；`firmware()` 返回简单、拥有所有权的节点元数据；`build()` 只能通过 `mmio("slot")`、`pio("slot")`、`irq("slot")`、`msi("slot")` 等接口消费计划签发的 claim。模型自己的类型化配置捕获在具体结构体中，不保存原始 TOML，也不重新查找 factory。

这里删除的是“每新增设备就增长”的设备类型 enum。`DeviceNodeKind`、`ResourceRequest`、trigger/sharing 等封闭且稳定的领域枚举继续保留。

## 设备图

每个节点拥有稳定 `DeviceNodeId`、可选父节点、显式依赖、dyn 模型和固件能力。节点种类为：

- `Virtual`：完全由 Axvisor 实现；
- `HostPassthrough`：保留规范化 host 固件身份和固定映射；
- `HostReplacement`：沿用 host 地址、中断和固件身份，但运行时由虚拟模型替换，例如 VGIC；
- `FirmwareOnly`：只参与 FDT/ACPI 的总线、容器或 provider。

图拒绝重复 ID、缺失依赖、重复依赖和环。封口后形成稳定拓扑顺序，不允许继续添加节点。passthrough 节点保存拥有所有权的规范化值，不保存 parser 引用、裸指针或任意 host AML 字节片段。

## 资源命名空间与确定性分配

资源包括：

- MMIO、PIO 区间；
- `(InterruptControllerId, ControllerInputId)`；
- `HostIrqId`；
- 按 ITS 隔离的 MSI DeviceID/EventID；
- controller-global LPI。

自动池、固定资源允许范围和架构保留区分开表达。一次规划按以下顺序执行：

1. 校验节点、slot、大小、对齐、范围和整数溢出；
2. 加入架构保留区与 host passthrough 固定资源；
3. 先放置全部 `Fixed` 请求；
4. 按节点 ID、资源种类和 slot 对 `Auto` 请求稳定排序；
5. 在对应命名空间 lowest-first 分配；
6. 全图成功后才发布 claim。

资源冲突在原子占用时检查，错误包含命名空间、资源值、已有所有者和新请求者。不同 controller 的同号 input 不冲突。共享电平线必须具有一致 trigger/sharing。

不采用 `vm-allocator`：它不能直接表达 owner 诊断、跨中断域命名空间、固定优先、共享 IRQ、MSI/LPI 复合资源、一次性 claim 和 VM 事务回滚。私有的区间查找以后可以替换，不影响公开领域模型。

## claim、lease 与 bundle 事务

资源槽只能经历 `planned -> issued -> leased`。每个节点一次取得 `ResourceClaimSet`，按名称消费后直接生成 lease，不再为每个槽构造一层 claim 对象。重复签发或重复消费失败；未消费 claim 不能完成构建。构建或 bundle 注册失败时，endpoint 与 lease 一起释放，资源恢复到 `planned`，相同输入可再次得到同一个最低资源。

`DeviceBuildContext::irq()` 根据 controller ID 找到已注册的 `VirtualInterruptController`，取得 `WiredIrqInput` 并为当前设备创建独立 `IrqLine`。edge 只使用 `pulse()`；level 使用 `assert()/deassert()`；shared-level 按 source 聚合为 wired-OR，source drop 自动撤销断言。

`DeviceBundle` 原子提交设备、controller、endpoint、typed service、grant、poller、lifecycle 和资源 lease。任一步失败都会恢复所有索引。controller bundle 必须先于依赖节点构建；全部节点成功且所有 claim 转为 lease 后才 seal runtime，seal 后拒绝注册。

## 固件模型

节点不再挂第二组 FDT/ACPI dyn trait。`DeviceModel::firmware()` 只返回 `DeviceFirmwareSpec`：节点名、compatible/HID、资源槽和简单属性。架构 composer 将它与同一节点的 `ResolvedDeviceResources` 组合成最终 FDT/AML；固件代码不能重新分配资源、按设备类型匹配或 downcast 运行时设备。

串口等简单设备直接使用这份通用元数据。GIC、ITS、PCI、IOAPIC 等特殊拓扑仍由架构计划处理，因为 phandle、MADT、PCI `_PRT` 和系统寄存器属于架构事实；特殊实现仍只能读取 resolved graph。

## Host 派生的默认串口

默认串口按 `machine fallback -> host FDT/ACPI snapshot -> console0 -> 用户同 ID 覆盖` 解析。FDT snapshot 保存所选 UART 的型号、reg、IRQ、clock、节点路径、phandle、clock provider 和 stdout identity；ACPI snapshot 保存 SPCR 的型号、地址空间、reg、IRQ、clock、baud 和 namespace。snapshot 拥有全部数据，不保存 parser 引用或 AML 字节。

用户不写 `console0` 时保留 host 优先和 machine 兜底行为。同 ID 请求完整替换 model/options：型号和 transport 兼容时继续使用 host fixed binding 与 identity，不兼容时丢弃 host identity，按普通虚拟设备自动分配资源。其他 ID 新增串口。每个 VM 最多一个 `host-console` backend owner，当前不能关闭默认串口。

VGIC、vPLIC、IOAPIC 等控制器不是 catalog 设备；它们由架构创建并先注册到同一个 `DeviceRuntime`。所有 MMIO/PIO exit 只查询 runtime 区间索引一次并调用 `Arc<dyn Device>::access`，未命中后的行为由架构策略决定。VGICv3 ICC 等系统寄存器仍属于 vCPU binding，不伪装成 MMIO。

透传 VM 以 host identity map 为基线，再扣除客户机 RAM、启动数据、虚拟 MMIO、host replacement 捕获区和架构保留区。LoongArch 等架构不得在早期代码中再次枚举“虚拟设备地址”；最终设备图统一完成扣洞。无法表示的重叠直接导致启动失败。

## 架构策略

- AArch64 先创建 VGIC host replacement，再加入串口、共享 provider 和配置设备；同一 `ArmVgicConfig` 驱动 VGIC 与 FDT。主线 timer、LR 和物理 SPI 生命周期保持权威。
- RISC-V 保留 PLIC hart/context 顺序，设备图只提供资源和注册事务。
- x86 保留 LAPIC、IOAPIC、PIT、APIC access 和 PCI 路由顺序；直接启动 ACPI 与 fw_cfg ACPI 读取同一解析后计划。
- LoongArch 保留 IOCSR、EXTIOI/PCH-PIC/PCH-MSI 级联和 MMIO fw_cfg；透传扣洞从最终图取得。

## 失败、锁与验证

领域错误使用 workspace `thiserror`，按配置、catalog、图、资源、构建和固件阶段区分。通用路径不猜测 controller、不忽略未知设备、不回退旧描述。

资源/registry 锁不在设备或控制器状态锁内获取；设备模型构建发生在 VM 可运行前。回调、唤醒、IPI 和物理 IRQ 操作遵守具体控制器的锁外执行契约。

测试只保留守住边界的节点：确定性分配、命名空间/共享规则、claim 与 bundle 回滚、catalog 错误，以及一个跨 crate 的配置 → dyn 图 → 固件 → runtime 集成场景。架构 QEMU 用例继续验证 AArch64 VGIC、x86 ACPI 与四架构启动。
