# 基于解析后设备图的 AArch64 VGIC 与中断拓扑

状态：PR #1718 的实现基线

## 范围与参考

通用设备图、dyn 模型、资源、claim、runtime 和固件契约见 `axvisor-resolved-device-graph.md`。本文只规定 AArch64 非安全 Group1 客户机需要的 GICv2、GICv3、ITS/LPI、软件中断和预配置物理 SPI 路径。

语义参考 Arm IHI 0048、Arm IHI 0069、Linux v7.1 KVM VGIC 文档和 QEMU 10.1.0。PR #1612 只作为设计和测试参考，不复制其与当前 `dev` 冲突的 machine/provider/SCMI 等实现。

Secure Group0/Group1S、GICv3.1 ESPI、GICv4/vPE、嵌套虚拟化和外部 live-migration 格式不属于本次范围。

## 分层原则

```text
axdevice_base
  类型化中断 ID、IrqLine、控制器能力 trait
        ^
axdevice
  dyn DeviceModel、设备图、资源、claim、bundle、runtime
        ^
axvm::arch::aarch64
  host FDT 解析、ArmVgicConfig、固件、vCPU 与物理 IRQ 生命周期
        ^
arm_vgic / arm_vcpu / host GIC backend
```

通用层不规定四架构的设备顺序。AArch64 自己完成 host GIC/FDT 解析、VGIC 计划、控制器 bundle 注册、普通设备构建、vCPU binding、物理 SPI backing、地址空间和 vCPU setup。RISC-V、x86 与 LoongArch 保留各自完全不同的顺序。

资源、固件身份、VGIC 配置和可变运行时状态按变化原因拆分，不建立同时持有所有内容的巨型 `VmPlan`。AArch64 计划由 `VmDevicePlan`、VGIC construction plan 和 firmware plan 等小组件组合。

## 状态所有权

每个 VM 只有一个 `Arc<VgicCore>`。enable、pending latch、当前 line level、active、priority、group、trigger、target/route 和 backing 都只保存在该 core 中。GICD、GICC/GICR、ICC 和 ITS 前端只负责架构访问解码，不保存第二份状态，也不向 host GICD/GICR 透传客户机写入。

同一个 `Arc<VgicCore>` 同时以具体类型服务 vCPU、EOI 和物理 backing，并转换为 `Arc<dyn VirtualInterruptController>` 注册给设备框架。这里没有转发状态机和 `InterruptFabric`。

`WiredIrqInput` 只表达电气 source 聚合：edge 调用 `pulse()`；level 调用 `assert()/deassert()`；shared-level 使用 wired-OR；source drop 自动撤销断言。LR 只是规范状态的有限硬件缓存，满时保留在软件 overflow 队列，不 panic。

## 配置与不可变计划

AArch64 在最终客户机 FDT 生成前完成两阶段计划：

1. 解析 host GIC、ITS、串口、timer、vCPU affinity 和物理 IRQ，净化为拥有所有权的 machine/firmware profile；
2. 创建 `ArmVgicConfig`、设备图和资源计划，之后禁止重新探测、重新分配或从 guest DT 反向恢复配置。

`ArmVgicConfig::{V2,V3}` 包含 GICD/GICC 或 GICD/GICR region、全部 GICR region、stride、vCPU affinity、SPI/LPI 容量、LR/priority 能力、ITS 和固定 `AssignedSpiConfig`。host 与 guest GIC 版本必须一致，跨版本直接拒绝。

GICv2 最多支持 8 个 vCPU。GICv3 校验 MPIDR affinity 唯一性、GICR frame 容量、region 不重叠和 stride。host 没有可用 ITS 时，配置不包含 ITS，固件不生成 ITS，任何 MSI 需求明确失败。

## VGIC 作为 dyn host replacement

设备图中的 VGIC 节点是 `HostReplacement`，保存同一个 VGIC model 实例。`requirements()` 为 distributor、每段 CPU-interface/redistributor region 和每个 ITS 声明命名 fixed MMIO slot；`build()` 逐项消费并校验解析后的范围，然后创建 VGIC frontends、系统寄存器适配器和 `ControllerRegistration`。

host replacement 沿用 host 固件地址、GICR/ITS 布局和中断身份，客户机只修改虚拟状态。所有 VGIC MMIO 仍由 stage-2 捕获，不直接映射 host 寄存器。普通虚拟设备只依赖 dyn controller，通过自动分配或平台 fixed slot 获得中断线。

注册顺序为：

1. 创建并注册 VGIC bundle；
2. 构建普通 IRQ/MSI 设备；
3. 为每个 vCPU 建立类型化 VGIC binding；
4. 建立物理 SPI backing；
5. 映射地址空间并完成 vCPU setup。

不允许遍历设备并 downcast 某个 GIC frontend 补配置。

## 资源命名空间与 claim

MMIO、PIO、`(controller,input)`、host IRQ、按 ITS 隔离的 DeviceID/EventID 和 controller-global LPI 是不同命名空间。AArch64 resource pool 提供自动 MMIO 和可分配 SPI；VGIC 内部范围、timer PPI、host replacement 和物理 SPI 在自动分配前保留。

fixed 请求先占用，auto 请求按节点 ID、种类和 slot 稳定排序并 lowest-first 分配。冲突检查发生在原子 reserve/claim，不执行 `is_free()` 后再占用。错误包含资源域、值、已有所有者和请求者。

claim 只能从 `planned` 进入 `issued` 再进入 `leased`。构建失败、未消费或 bundle 注册失败会回滚；重新构建得到相同最低资源。endpoint registration 与 lease 一同进入 bundle，并与 VGIC controller 索引原子提交。

不采用 `vm-allocator`，因为这里还需要 owner 诊断、跨 controller 域、共享 IRQ 兼容、MSI/LPI 复合占用和 VM 事务语义。私有 lowest-first 搜索以后可以独立替换。

## GICv2/GICv3 主路径

主线 `VgicCore` 继续负责常规非安全客户机路径：

- v2：GICD、GICC、SGI source、PPI、SPI、CPU target、priority、pending/active、EOI/DIR；
- v3：GICD、每 vCPU GICR、ICC 系统寄存器、affinity routing、SGI/PPI/SPI；
- ITS/LPI：host 能力存在时使用同一 VM-local core 与检查过的 guest memory。

保留位和不支持的安全扩展按架构 RAZ/WI，不使用 `todo!`、panic 或 host register passthrough。每个 INTID 明确区分 pending latch 与仍有效的 line level。

vCPU 路径为：

```text
fold 已保存 LR -> refill -> restore -> guest run -> save -> fold
```

pause/resume 保存 HCR、VMCR、APR 和全部 LR。主线虚拟 timer、maintenance、EOI/DIR 和 LR overflow 实现保持权威，不在本分支复制另一套状态机。

## 物理 SPI

物理 SPI 在计划阶段固定，强制 `guest INTID == host INTID`。host IRQ identity、物理 trigger 和 route 对客户机不可修改。host acknowledge 后进入同一 VGIC 状态，只有客户机 EOI/DIR 后才执行正确的 host deactivate。

host route 使用 AArch64 平台现有 route slot 和 VM 生命周期状态，不增加第二个通用 host IRQ registry。重复物理 SPI 在 VM 可运行前失败。停止、quiesce、drain、deactivate 和销毁沿用主线生命周期。

## FDT

VGIC runtime 与 FDT 使用同一个不可变 profile/config：

- v2 保留 host GICD/GICC 地址，删除 GICH/GICV、maintenance、安全和 host-only 属性；
- v3 保留 host GICD、全部 GICR region、stride 与 cell 描述；
- ITS 只在 host 能力存在时生成；
- phandle 保持或一致重写；
- host replacement 与物理 SPI 保持地址和 INTID 身份。

普通设备固件模型读取解析后的 MMIO/IRQ/MSI slot，不重新分配。固件中出现的每个数字必须能回溯到 `ResolvedDeviceGraph` 或不可变架构计划。

## 锁顺序与锁外动作

锁顺序为：

```text
resource claim -> device/controller registry -> VGIC state -> backend LR/route state
```

持有 VGIC 状态锁时只更新规范状态并产生待执行动作。唤醒、IPI、maintenance 通知、host acknowledge/deactivate 和物理 route 操作都在锁外执行。资源/registry 锁不能在 VGIC 锁内获取。

## 失败与验证矩阵

规划和注册都是全成或全退。VGIC 构建失败释放 endpoint 与 lease；物理 backing 失败先撤销 host route，再销毁注册。固件序列化只读取不可变资源，因此不能描述与 runtime 不同的地址、IRQ、GICR 或 ITS。

| 区域 | 必要证据 |
| --- | --- |
| planner | fixed 优先、输入顺序无关、lowest-first、溢出/耗尽、失败后最低资源重试 |
| namespace | 跨 controller 同号、exclusive 冲突、shared trigger 不匹配、ITS 隔离、全局 LPI |
| claim/runtime | 重复/未消费、bundle 回滚、controller 缺失/重复/ID 不匹配、seal |
| VGIC | SGI/PPI/SPI、line/latch、pending/active、priority/route、overflow、maintenance、EOI/DIR |
| firmware | VGIC config、设备资源、GIC/ITS/interrupts 来自同一计划 |
| physical | 固定 identity/trigger/route、重复 host SPI、quiesce/drain、EOI/DIR 到 deactivate |
| system | QEMU GICv2 2-vCPU、GICv3+ITS 4-vCPU timer stress 和四架构 smoke |
