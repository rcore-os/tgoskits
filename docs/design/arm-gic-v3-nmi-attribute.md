# Arm GICv3 NMI 属性编程

## 问题与目标

本仓库内置的 `arm-gic-driver` 已定义 `GICD_INMIR<n>` 和
`GICR_INMIR0` 寄存器，也有仅供 Distributor 使用、对失败静默忽略的 SPI
helper，但没有可供平台层安全使用的完整能力：SGI/PPI 无法配置，调用者无法判断
控制器是否实现 `FEAT_GICv3_NMI`，当前 PE 的 Redistributor 查找会丢弃
MPIDR Aff3，错误 INTID 或缺失 Redistributor 也没有可匹配的错误。

目标用户是需要在 AArch64 GICv3 平台上配置硬件 NMI 属性的 OS/platform glue。
完成标准是：处于 disabled/inactive 状态的标准 SGI、PPI、SPI 能按 Arm 定义的
寄存器槽位设置和查询属性；能力、INTID、Group、affinity routing、运行状态和
Redistributor 错误均显式返回；原有普通 IRQ 路径保持不变。

## 规范基线

实现以 *Arm Generic Interrupt Controller Architecture Specification*,
Arm IHI 0069H.b（2024-04-12）为准：

- GICv3.3 引入 non-maskable property，`GICD_TYPER.NMI` bit 9 是
  `FEAT_GICv3_NMI` 的能力来源；
- `GICD_INMIR<n>` 使用 `n = INTID DIV 32`、`bit = INTID MOD 32`，
  其中 Distributor 的 SGI/PPI 位为 RES0；
- SGI/PPI 使用当前 PE 的 `GICR_INMIR0`；
- Group 0、对应 Security state 未启用 affinity routing、未实现的中断或
  Non-secure 不可访问的 Secure 中断位是 RES0/RAZ-WI。
- 写 `INMIR` 时，pending 中断可以采用旧属性或新属性，但实现必须保证中断不丢失、
  不重复处理且变化在有限时间内可见；SGI 是否可禁用由实现定义。

因此生产代码只读取 `GICD_TYPER.NMI`。当该位为零时，写读回探测既不能发现
规范之外的能力，还会受 Group、Security state 和 affinity routing 影响，并会
临时改变真实中断的属性，所以不采用该方案。

## 接口与所有权

`NmiAttribute::{Maskable, NonMaskable}` 代替布尔参数，避免调用点隐藏写入方向。
`Gic::set_nmi_attribute(&mut self, ...)` 使寄存器读改写的独占要求体现在类型上；
`Gic::nmi_attribute(&self, ...)` 提供对应查询；
`Gic::supports_nmi_attributes()` 只报告架构能力位，不宣称已经具备完整 NMI
异常入口、acknowledge 或运行时处理链。

访问前依次验证标准 INTID 槽位、能力位、实现的 SPI 范围、可访问 Group 1 和
对应 affinity routing。私有中断只访问当前 PE 的 Redistributor，查找使用完整的
Aff3:Aff2:Aff1:Aff0；缺失帧返回 `NmiAttributeError`。调用者仍须先完成
Distributor 和当前 Redistributor 初始化，并遵守 `Gic::new` 的唯一硬件所有者
契约。setter 在写入前读取同一 INTID 的 `ISENABLER` 和 `ISACTIVER`，对 enabled
或 active 状态分别返回明确错误；pending 但 disabled/inactive 的中断仍可设置。
调用者还须在调用期间序列化独立 MMIO 别名和该 INTID 的中断处理。对硬件永久启用
的 SGI，本接口会返回 `InterruptEnabled`，不会以不安全写入伪造支持。

公开接口的调用顺序由 `examples/gicv3_nmi_attribute.rs` 编译检查。setter 和
getter 都要求先完成 Distributor 初始化，以使用 `Gic::init` 确认的 Security
state；访问 SGI/PPI 时还要求先初始化当前 PE 的 Redistributor。

## 方案比较

- 升级外部依赖不可行：本仓库通过 workspace path 维护
  `drivers/intc/arm-gic-driver`，并非 crates.io 或 git 依赖。
- 原样复制外部 PR 的写读回能力探测不符合上述架构能力语义，也可能影响活动中断。
- 继续暴露静默 no-op 的原始 SPI helper 无法覆盖 SGI/PPI，也无法让平台层区分
  unsupported、无效输入和拓扑错误。
- 选择在本地驱动边界提供小型 typed API，并把寄存器槽位映射保留为可在 host
  稳定测试的纯逻辑。

## 范围与非目标

本次只支持标准 INTID 0-1019。Extended SPI/Extended PPI 的
`GICD_INMIR<n>E`/`GICR_INMIR<n>E`、`ICC_NMIAR1_EL1` acknowledge、异常入口、
优先级和端到端 NMI delivery 不在本次范围内；这些能力需要独立的调用方、接口和
运行时设计。现有 `somehal` 普通 IRQ API 不会被隐式改成 NMI。

## 验证与回滚

最低层 host 回归固定验证 SGI/PPI/SPI 到 INMIR 的映射，并拒绝 special/extended
INTID；该测试在旧实现中因缺失映射能力而确定性编译失败。AArch64 Linux 用户态
测试在 QEMU user mode 中以假 MMIO 验证 capability、Group、affinity routing、
未实现 SPI、enabled/active 拒绝、设置/清除/读回以及包含 Aff3 的 Redistributor
匹配。AArch64 Linux target 还会编译 `gicv3_nmi_attribute` example，验证公开 API
的初始化、能力检查、禁用、设置与查询顺序。另以 bare-metal AArch64 target
clippy/check 和现有 GICv3 QEMU 用例验证目标编译与普通 IRQ 路径。

变更不修改初始化默认值或普通 IRQ 行为。若需回滚，只需移除新增 API、能力位定义
和测试；调用者在能力缺失时已经得到显式 `Unsupported`，无需迁移持久状态。
