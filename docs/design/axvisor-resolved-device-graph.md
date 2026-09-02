# Axvisor 解析后设备图与客户机固件

状态：当前实现基线

本文描述设备图、资源和固件的所有权边界。架构共同能力、部分能力、默认行为与单一实现的放置规则，见[《AxVM 分层能力接口设计》](./axvm-capability-layering.md)。两份设计共同约束 AxVM：能力接口回答“谁能做什么”，解析后设备图回答“已经分配了什么、由谁持有”。

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
model = "virtio-blk"
capacity = "20GiB"
backend = "file"
path = "/images/data.raw"
```

`VirtualDeviceRequest` 只保留稳定 ID、规范 model 名和剩余 TOML table。它禁止用户填写 MMIO、PIO、IRQ、MSI 或 LPI 数字。catalog 中的 `ConfiguredModelRegistration` 保存规范 model 名和普通构造函数指针；构造函数使用带 `deny_unknown_fields` 的类型化结构解析 options，并直接返回 `DeviceNodeSpec`。未知 model、重复注册、重复设备 ID 和未知选项都明确失败，不存在额外 factory trait 或 instance 包装层。

catalog 由代码显式构造；`ConfiguredDeviceCatalog::new()` 始终为空，不隐藏任何默认 model。`axvm::machine::register_devices()` 通过普通 Rust `mod` 和函数调用注册 AxVM 拥有的 serial、IVC、virtio-blk 与 virtio-net，并以事务方式提交整批 registration；任一 model 冲突时 catalog 保持调用前状态。应用只把 VM 请求和必填 catalog 传入 `AxVMConfigParams`。注册记录 `module_path!()` owner，重复 model 会同时报告原 owner 与冲突 owner。不使用 linker section、全局静态发现、宏扫描、动态库或外部插件。

通用设备实现位于 `virtualization/axvm/src/configured/devices/`。新增只使用现有资源类型和固件 contribution 的通用设备时，只增加设备模块，并在该目录的显式注册文件添加 `mod` 与 `register()` 调用；不修改 Axvisor、VM config assembly、四架构 VM-exit 或 FDT/ACPI composer。

`DeviceInstantiationContext` 只暴露架构和默认 wired/MSI 域等小型能力，不暴露架构内部对象、裸 IRQ 或设备管理器。

## dyn 设备模型

图节点保存声明和构建都使用的同一个 `Arc<dyn DeviceModel>`：

```rust
pub trait DeviceModel: Send + Sync {
    fn requirements(&self) -> DeviceManagerResult<DeviceRequirements>;

    fn firmware(&self) -> DeviceFirmwareSpec;

    fn build(
        &self,
        context: &mut DeviceBuildContext<'_>,
    ) -> Result<DeviceBundle, DeviceManagerError>;
}
```

`requirements()` 是纯声明阶段，只能返回命名资源槽和能力需求；`firmware()` 必须显式返回 `None`，或者 `Interfaces { fdt, acpi }`。FDT-only、ACPI-only 和双支持都可表达；两个 interface 都缺失或已声明 interface 的 contribution 为空会在 graph declaration 阶段失败。`DeviceNodeSpec` 创建时只调用一次 `firmware()` 并冻结结果，后续 resolved graph、composer 与 runtime 不再回调 model。`build()` 只能通过 `mmio("slot")`、`pio("slot")`、`irq("slot")`、`msi("slot")` 等接口消费计划签发的 claim。模型自己的类型化配置捕获在具体结构体中，不保存原始 TOML，也不重新查找 factory。

这里删除的是“每新增设备就增长”的设备类型 enum。`DeviceNodeKind`、`ResourceRequest`、trigger/sharing 等封闭且稳定的领域枚举继续保留。

这不是过渡状态，也不应机械地把剩余枚举改成能力接口。设备访问、轮询、中断控制和生命周期表示“对象能做什么”，适合由实现关系表达；节点种类、资源申请方式和解析结果表示“规划到了哪一步、资源归谁”，适合由封闭数据类型穷举。模型同时负责需求声明和按计划构建，是为了保证两阶段消费同一事实来源，而不是一个等待拆开的过大接口。

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

`VmDevicePlan` 在自动 MMIO 分配前保留 guest memory。`reserved_address_ranges` 先减去已由 guest memory 或 fixed host replacement 接管的部分，再合并为互不重叠的 reservation。host passthrough 和其他 fixed 请求先于自动请求放置，因此自动设备会跳过这些范围。host FDT 的 `/reserved-memory` 只导入缺少 `status` 或 `status = "ok"`、`"okay"` 的 direct child；disabled 节点不占用客户机资源池。

不采用 `vm-allocator`：它不能直接表达 owner 诊断、跨中断域命名空间、固定优先、共享 IRQ、MSI/LPI 复合资源、一次性 claim 和 VM 事务回滚。私有的区间查找以后可以替换，不影响公开领域模型。

## claim、lease 与 bundle 事务

资源槽只能经历 `planned -> issued -> leased`。每个节点一次取得 `ResourceClaimSet`，按名称消费后直接生成 lease，不再为每个槽构造一层 claim 对象。重复签发或重复消费失败；未消费 claim 不能完成构建。构建或 bundle 注册失败时，endpoint 与 lease 一起释放，资源恢复到 `planned`，相同输入可再次得到同一个最低资源。

`DeviceBuildContext::irq()` 根据 controller ID 找到已注册的 `VirtualInterruptController`，取得 `WiredIrqInput` 并为当前设备创建独立 `IrqLine`。edge 只使用 `pulse()`；level 使用 `assert()/deassert()`；shared-level 按 source 聚合为 wired-OR，source drop 自动撤销断言。

`DeviceBundle` 原子提交设备、controller、endpoint、typed service、grant、poller、lifecycle 和资源 lease。任一步失败都会恢复所有索引。controller bundle 必须先于依赖节点构建；全部节点成功且所有 claim 转为 lease 后才 seal runtime，seal 后拒绝注册。

## typed PCI function 与 root binding

普通模型通过 `DeviceRequirements::with_pci_function` 声明至多一个 `PciFunctionRequirement`：typed host key、`PciEndpointIdentity`、`Auto`/`Fixed` BDF 请求和 32-bit non-prefetchable memory BAR 列表。每个 architecture composition root 拥有唯一 `PciHostProvider`（host 节点、memory aperture slot、平台 function spec 及保留 BDF），不使用全局 registry、model 字符串发现或隐式默认 provider。x86 始终通过 `DeviceGraphBuilder::register_pci_host` 注册 Q35 provider；AArch64 的 `VmDevicePlan::with_optional_pci_host_for_vm` 先检查 typed requirements，只在 endpoint 引用了对应 host key 时注册 provider。graph declaration 阶段按 host key 解析 provider 并自动追加 endpoint→host 依赖边；自动边与显式边进入同一拓扑排序与环检测，缺失或重复 provider 在 declaration 阶段返回 typed error，调用方不得手写该依赖。

PCI topology 在 `ResolvedDeviceGraph` 的同一解析事务内消费已规划的 host aperture：先校验 fixed 请求与平台保留 BDF，再按稳定节点 ID 为 auto function 分配最低空闲 device 的 function 0，BAR 按 size 降序 + node ID + index 确定性 first-fit，最后生成 power-on config image。每个 resolved function 携带非空的 owner（configured endpoint 归 endpoint node，Q35 host bridge/LPC 等平台 function 归 host node）和所属 host node id；metadata 仅在全图成功后一次性发布到 `ResolvedDeviceGraph`，不产生第二个公开 graph 或 seal。x86 上 Q35/LPC 与 endpoint 共享同一 `PciRootState`：CF8/CFC frontend 只解码 configuration mechanism #1 端口访问并统一走 root lookup，不保存 function table 或固定 BDF；完整 memory aperture 是单一顶层 MMIO 设备，BAR relocation 只改 root 内部 route；ACPI `_CRS` 的 PCI memory window 从同一 resolved aperture 推导并显式校验一致。

AArch64 使用同一个 typed provider 和 root binding。没有 endpoint 时，graph 不实例化 host 节点，也不申请 ECAM、memory aperture 或 runtime root service。存在 endpoint 时，host 从 `0x0b00_0000..0x1_0000_0000` 自动 MMIO 搜索域取得 1 MiB ECAM 与完整 64 MiB memory aperture；lowest-first 分配会避开 guest memory、有效 reserved-memory、剩余 `reserved_address_ranges`、fixed virtual resources 和 passthrough mappings。ECAM frontend 和 aperture frontend 只委托同一 `PciRootState`，架构固件适配器从 graph-resolved ranges 生成 `pci-host-ecam-generic` 节点。已有 PCI bridge、冲突 top-level `reg`、无 DTB endpoint、4 GiB 以下无合法窗口或 host range 契约无效时明确失败，不覆盖或猜测 fallback。

runtime 注册时，host bundle 必须恰好发布一个 `PciRootBinding` 服务，runtime 校验其 host node id 与 `Arc` 身份均匹配本节点 resolved topology；endpoint bundle 声明 bundle-local device index 实现 resolved function，runtime 只沿冻结的 host dependency 取得该 binding，分配最终 endpoint `DeviceId` 并生成不可伪造的 `EndpointRouteToken`（仅含 `DeviceId` + binding generation，不携带任何 capability），经 binding 原子写入 root 后把 lease 保存在 runtime 中。缺失、重复、owner 不匹配、来自非 dependency 节点的 root service 或 bind 失败都会回滚整个 bundle 注册；lease drop 按“先失效 generation、再撤销 root route、最后释放引用”的顺序完成解绑。root 使用窄的 IRQ-safe 锁只保护 config bytes、BAR route 和 binding bookkeeping；token 与 route 在锁内克隆，回调严格在锁外由 runtime 校验 token 后执行。旧 generation token 在 unbind 或 rebind 后永远失效。

当前 BAR dispatch 把身份正确但无能力的 context 传给 endpoint 回调：endpoint 的 DMA/timer/wake/stop grant 按 `DeviceId` 存放在 `DeviceRuntime` 中，BAR 回调路径暂不可达。这是记录在案的阶段限制而非长期语义——路由与 dispatch guard 的归属是 `DeviceRuntime`，由其基于 token 的 endpoint `DeviceId` 构造 endpoint-scoped 的真实 `DeviceContext`；首个需要 grant 的 endpoint 必须在自己的设计中扩展该 seam，并以 grant 经 BAR 回调实际生效的回归测试交付。token 本身永不签发或代理能力。

## 固件模型

节点不再挂第二组 FDT/ACPI dyn trait。`DeviceModel::firmware()` 返回由 `FdtContributionSpec` / `AcpiContributionSpec` 分类的 typed contribution：普通设备、中断控制器、timer、PCI host bridge、console 或 firmware transport。架构选择 FDT 或 ACPI 后，先验证图中每个 `Interfaces` 设备支持该接口，再把 contribution 中的 slot 解析为最终地址、IRQ 与 controller identity。固件代码不能重新分配资源、按 model 匹配、注入 raw DTS/AML 或 downcast 运行时设备。

普通设备由共享 composer 编码。FDT 使用 resolved register/interrupt slot 生成唯一的 unit-address 节点；ACPI 为多实例设备分配唯一 NameSeg 和 `_UID`。GIC、ITS、PCI、IOAPIC 等特殊拓扑仍由架构 adapter 编码，因为 phandle、MADT、PCI `_PRT` 和系统寄存器属于架构事实；但对应 model 同样必须声明 typed contribution，adapter 只能读取 resolved graph，不以空 firmware 默认值暗示“架构自行处理”。平台选择的接口缺失时立即失败，不回退到另一接口，也不静默忽略。

`InterruptController` contribution 显式携带运行时 `InterruptControllerId`。解析后的 FDT/ACPI 中断保留 `(controller, input)` 两个维度：FDT adapter 将 controller 映射到对应 phandle，ACPI adapter 将 controller-local input 通过架构声明的 GSI base 映射到系统 GSI。只有单控制器的架构也必须校验 controller identity；不匹配时拒绝 VM，不能绑定默认 parent 或直接把 input 当作 GSI。

固件解析会解析每一个特殊 contribution 的全部 register、interrupt 和属性 slot。架构 adapter 必须按 contribution 分类与固件身份消费所有特殊项，并在重复、缺失或尚不支持的分类出现时明确失败；不得用 graph 节点 ID 代替 contribution 选择，也不得通过过滤 `Conventional` 后 `continue` 来丢弃其余分类。

## Host 派生的默认串口

默认串口按 `machine fallback -> host FDT/ACPI snapshot -> console0 -> 用户同 ID 覆盖` 解析。FDT snapshot 保存所选 UART 的型号、reg、IRQ、clock、节点路径、phandle、clock provider 和 stdout identity；ACPI snapshot 保存 SPCR 的型号、地址空间、reg、IRQ、clock、baud 和 namespace。snapshot 拥有全部数据，不保存 parser 引用或 AML 字节。

用户不写 `console0` 时保留 host 优先和 machine 兜底行为。同 ID 请求完整替换 model/options：型号和 transport 兼容时继续使用 host fixed binding 与 identity，不兼容时丢弃 host identity，按普通虚拟设备自动分配资源。其他 ID 新增串口。每个 VM 最多一个 `host-console` backend owner，当前不能关闭默认串口。

VGIC、vPLIC、IOAPIC 等控制器不是用户 catalog 设备；它们由架构在一处显式创建，并先注册到同一个 `DeviceRuntime`。所有 MMIO/PIO/SysReg 设备访问只查询 runtime 区间索引一次并调用 `Arc<dyn Device>::read/write`，未命中后的行为由架构策略决定。VM-exit 不按设备类型、model、ID 或固定地址分派，也不 downcast。VGICv3 ICC 等 vCPU architectural interface 仍属于 vCPU binding，不伪装成 MMIO。

vPLIC 在自己的 `Device::read/write` 成功后发布 VSEIP 状态；LoongArch PCH-PIC 的 runtime wrapper 在自己的访问完成后发布 controller output。架构 VM-exit 因而不再执行设备专属 post-access hook。架构仍保留 exit-reason match、指令解码、寄存器写回、x86 未映射端口语义，以及 RISC-V/LoongArch nested-page-fault fallback。

透传 VM 以 host identity map 为基线，再扣除客户机 RAM、启动数据、虚拟 MMIO、host replacement 捕获区和架构保留区。LoongArch 等架构不得在早期代码中再次枚举“虚拟设备地址”；最终设备图统一完成扣洞。无法表示的重叠直接导致启动失败。

## 架构策略

- AArch64 先创建 VGIC host replacement，再加入串口、共享 provider 和配置设备；同一 `ArmVgicConfig` 驱动 VGIC 与 FDT。主线 timer、LR 和物理 SPI 生命周期保持权威。
- RISC-V 保留 PLIC hart/context 顺序，设备图只提供资源和注册事务。
- x86 保留 LAPIC、IOAPIC、PIT、APIC access 和 PCI 路由顺序；直接启动 ACPI 与 fw_cfg ACPI 读取同一解析后计划。
- LoongArch 保留 IOCSR、EXTIOI/PCH-PIC/PCH-MSI 级联和 MMIO fw_cfg；透传扣洞从最终图取得。

## 失败、锁与验证

领域错误使用 workspace `thiserror`，按配置、catalog、图、资源、构建和固件阶段区分。通用路径不猜测 controller、不忽略未知设备、不回退旧描述。

资源/registry 锁不在设备或控制器状态锁内获取；设备模型构建发生在 VM 可运行前。回调、唤醒、IPI 和物理 IRQ 操作遵守具体控制器的锁外执行契约。

测试只保留守住边界的节点：确定性分配、命名空间/共享规则、claim 与 bundle 回滚、catalog 错误，以及一个跨 crate 的配置 → dyn 图 → 固件 → runtime 集成场景。PCI 侧同等保留 typed BDF/BAR 解析、config/BAR 状态机、root binding 与回滚矩阵、x86 CF8/CFC 兼容的边界测试。架构 QEMU 用例继续验证 AArch64 VGIC、x86 ACPI 与四架构启动；generic vPCI 枚举由 x86 `pci-enumeration` VMX/SVM 用例验证。
