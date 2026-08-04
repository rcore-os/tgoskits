# Axvisor AArch64 通用定时器虚拟化设计

## 状态

本文定义 Axvisor 使用的、不兼容旧实现的 AArch64 通用定时器模型，也是实现必须遵守的所有权与 world switch 契约。凡修改定时器状态、计数器 offset、PPI 完成、vCPU 迁移或固件资源，均须在合入前同步更新本文。

本文与合入最新 `origin/dev` 后的当前实现同步；实现契约变化时必须在同一变更中更新本文。

本设计统一适用于 QEMU GICv2、QEMU GICv3、RK3568、RK3588 及其他 AArch64 host。不得通过板卡名称、SoC compatible 或 GIC 版本特判修复问题。

## 问题

旧实现把同一个架构定时器拆给多个互不相关的所有者：

- 客户机定时器寄存器部分保存在 `GuestSystemRegisters`；
- VM 级 relay 复制部分 `CNTV` 状态；
- 一个总线设备形态的模拟定时器又拥有另一份软件模型；
- WFI timer wheel 独立推断唤醒；
- 虚拟 GIC 接收 PPI level，却不拥有完整的 pending/active/EOI 生命周期；
- `CNTV_CTL_EL0` 仍可能启用时就清除了 `CNTVOFF_EL2`。

最后一个顺序错误会暂时把客户机 CVAL 移入 host 物理计数器的 epoch。RK3568 上，客户机可见时间会从约 3.96 秒跳至 78.86 秒，随后启动超时。RK3568 与 RK3588 都使用 GICv3，因此 GIC 版本分支无法解释或正确修复该问题。

替代实现必须提供以下可观察属性：

1. 每个 vCPU 只有一份权威的虚拟定时器和物理定时器 context。
2. 同一 VM 的所有 vCPU 共享相同且不可变的计数器频率与 offset。
3. 客户机定时器或客户机计数器 offset 仍安装在硬件中时，不得运行任何 host Rust 代码。
4. 定时器输出是 level input；pending、active、enable、route、EOI 和 DIR 状态全部由 VGIC 独占。
5. 已 acknowledge 的 host CNTV PPI 只有在对应客户机投递退休后才 deactivate，显式迁移或 teardown 除外。
6. WFI 使用最早的可投递定时器 deadline，绝不把 stale callback 直接转成强制 PPI。
7. Host 固件与 runtime 消费同一份经过校验的定时器 profile。

## 参考模型

实现遵循 Linux commit `8cd9520d35a6c38db6567e97dd93b1f11f185dc6` 中 KVM nVHE 的所有权模型。这里故意固定参考快照，避免上游后续重构改变本文所引用的具体状态机与操作顺序：

- [`struct arch_timer_vm_data` 与 `struct arch_timer_context`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/include/kvm/arm_arch_timer.h) 将 VM offset 与每 vCPU 定时器 context 分离，并跟踪 context 是否已装载到硬件。
- [`timer_save_state`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/arch/arm64/kvm/arch_timer.c) 先读取 CTL/CVAL、禁用定时器并执行 ISB，之后才清除计数器 offset。
- `timer_restore_state` 先安装 offset 和 CVAL，再启用 CTL。
- `kvm_timer_blocking` 调度最早能够唤醒阻塞 vCPU 的定时器。
- `kvm_timer_vcpu_load_gic` 根据虚拟 GIC 的 active 状态协调定时器输出，而不是把 host PPI 当成独立的软件中断。

Axvisor 不引入 Linux hrtimer、nested virtualization、VHE 专用切换或 userspace irqchip fallback。它使用自身 timer wheel 和统一 VGIC controller 保持相同的所有权与顺序。

## 状态所有权

| 状态 | 所有者 | 说明 |
| --- | --- | --- |
| 计数器频率与 offset | `ArmTimerVmConfig` | 不可变，且同一 VM 的所有 vCPU 一致 |
| CNTV CVAL、ENABLE、IMASK | vCPU `ArmTimerContext` | 直接装载到硬件 |
| CNTP CVAL、ENABLE、IMASK | vCPU `ArmTimerContext` | 通过陷入的软件模拟访问 |
| ISTATUS | 派生状态 | 根据当前 counter 和 CVAL 计算；不可写也不保存 |
| 定时器输出 level | vCPU snapshot 与 `Aarch64TimerBinding` | 发布到一条私有 VGIC line |
| PPI pending/active/enable/EOI | `arm_vgic` | 权威中断状态 |
| 已 acknowledge 的 host CNTV token | host timer-PPI binding | 排他资源，保留至 VGIC 退休 |
| assigned-SPI pending/active 状态 | 物理 GIC 与 HW-backed LR | LR 携带经过所有权校验的物理 INTID |
| WFI 唤醒事件 | CPU-owned AxVM timer wheel | 仅为 hint；callback 不 assert PPI |
| FDT 中断身份 | `GuestTimerProfile` | runtime 绑定与 FDT 生成共同使用 |

通用 channel 不承载定时器 IRQ 状态。`IrqNotify` 与 timer wheel 只传递延迟工作或唤醒 hint。Hard IRQ 可以 acknowledge 并 priority-drop 中断源，再发布预分配的通知状态；不得查找 VM、分配内存、获取 `rdrive` 锁或调用 subscriber。

AArch64 hypervisor host 会在每个 GICv2 或 GICv3 CPU interface 上启用 split EOI 模式。因此 `EOIR` 只执行 priority drop，`DIR` 才是显式完成边界。普通 host IRQ dispatch 通过 `ActiveIrq::drop` 同时完成这两个操作，从而保持原有单阶段行为。AxVM 所有的 PPI 和 SPI 使用同一 host 模式，但会把已 acknowledge 的 activation 保留至虚拟中断退休。Timer PPI 把 opaque token 保存在 timer binding 中；assigned SPI 则通过 HW-backed LR 指明物理中断源，不再维护第二份软件 pending 状态。只在 AxVM fast path 启用 split EOI 是错误的，因为该模式属于 CPU interface 状态，并与普通 host IRQ dispatch 共享。

不属于客户机的已 acknowledge 中断仍沿普通 host IRQ 图处理。Raw GIC parent INTID 会先经已安装的 parent-to-leaf route 解析，再执行 dispatch，因此 ITS LPI 能到达其 MSI/MSI-X leaf handler。GICv3 发出 `DIR` 时保留完整的 24-bit 架构 INTID；只有经过独立校验的设备直通契约限制为可分配 SPI。

VGIC 退休会在释放 controller lock 后调用一个 typed backend 操作。这是单一所有者的生命周期边界，不是通用 callback registry，也不是第二条 pending queue。

## VM 配置与计数器域

`ArmTimerVmConfig` 包含：

- 一个客户机可见频率；
- 一个 VM-wide virtual offset；
- 一个 physical offset。

AxVM 记录每个已启用物理 CPU 的硬件 `CNTFRQ_EL0`。创建 VM 时，收集 vCPU placement 或 affinity mask 指定的所有 CPU，并拒绝：

- 空的目标 CPU 集合；
- 没有发布 capability snapshot 的目标 CPU；
- 为零的计数器频率；
- 不一致的计数器频率。

如果 host `arm,armv8-timer` 节点提供有效的 `clock-frequency`，该固件值作为客户机可见频率的纠正值；但所有目标 CPU 的硬件频率仍必须一致。

普通 EL1 客户机模型为：

- `CNTVCT = CNTPCT - virtual_offset`；
- CNTV 直接在硬件运行；
- CNTP/CNTPCT 访问陷入，并使用当前为零的 `physical_offset`；
- 暂停、调度离开和重启不重写 offset，因此客户机时间持续推进；
- 不支持 nested virtualization 和客户机 EL2 timer。

Stage-2 页表级数和 IPA 位宽取所有可能目标 CPU capability 的最小值。定时器和页表选择因而使用同一个完整 placement 集合，不能静默退化为创建 VM 时恰好所在 CPU 的能力。

## World Switch 事务

### 进入客户机

最终的纯汇编窗口执行：

1. 保存 host `CNTHCTL_EL2` 与 `CNTKCTL_EL1`；
2. 写 `CNTV_CTL_EL0 = 0`，随后执行 ISB；
3. 安装 VM `CNTVOFF_EL2`；
4. 安装客户机 timer trap policy 与客户机 `CNTKCTL_EL1`；
5. 安装 `CNTV_CVAL_EL0`，随后执行 ISB；
6. 安装可写的 `CNTV_CTL_EL0`；
7. 标记 timer context 已装载，随后执行 ISB；
8. 恢复客户机寄存器并 ERET，期间不调用 Rust。

VGIC 状态和当前 pCPU 的 timer-PPI route 在该事务前准备完成。

### 退出客户机

对于 lower-EL IRQ，异常汇编会在客户机定时器 level 仍 asserted 时先读取 host IAR，并只把 raw acknowledgement 保存到 vCPU 的 host-runtime slot。GICv2 使用 VGIC 构建期间发现的、不可变的 memory-mapped CPU interface 地址；GICv3 使用 `ICC_IAR1_EL1`。这是一个纯汇编操作：客户机定时器状态仍装载时，不运行 Rust、不查找 VM、不分配、不调用 callback，也不获取 controller lock。

随后，公共退出事务执行：

1. 把 CNTV CTL 和 CVAL 读入 vCPU context；
2. 保存客户机 `CNTKCTL_EL1`；
3. 写 `CNTV_CTL_EL0 = 0`，随后执行 ISB；
4. 清除 `CNTVOFF_EL2`；
5. 恢复 host `CNTHCTL_EL2` 与 `CNTKCTL_EL1`；
6. 标记 timer context 已卸载，随后执行 ISB；
7. 只有完成上述步骤后才调用 Rust。

恢复 host timer context 后，Rust 校验捕获的 IAR 值，执行 split-EOI priority drop，并把它转换为 opaque completion token。然后先发布当前 CNTV/CNTP level，再保存并合并 VGIC 状态。如果该退出源于 trapped DIR，那么客户机此前对 CVAL/CTL 的写入会在 deactivation 判断 level 是否需要重新 pending 前生效。

对于 level PPI，在写 `CNTV_CTL_EL0 = 0` 后才 acknowledge 是错误的。读取 `GICC_IAR` 或 `ICC_IAR1_EL1` 前，line 可能已经 deassert，造成 spurious INTID 和立即重新进入客户机的循环。该顺序对 GICv2、GICv3、QEMU 和真实开发板相同，不是平台 workaround。

## Timer PPI 生命周期

虚拟定时器和 non-secure physical timer INTID 来自 `GuestTimerProfile`，runtime 代码不使用固定常量。

CNTV host 中断处理分为以下阶段：

1. AxVM 在 hypervisor 生命周期内只 claim 一次架构 virtual-timer PPI，在每个 pCPU 上把它配置为 level-triggered，并为 host context race 保留一个固定且无分配的 fallback；
2. host GIC CPU interface 已处于 split EOI 模式；
3. lower-EL IRQ 汇编在停止 CNTV 前 acknowledge CPU-local PPI；
4. 切回 host 后的 Rust 对捕获的 acknowledgement 执行 priority drop，并记录其 opaque token 与 owner pCPU；
5. timer snapshot publication 更新 virtual PPI level；
6. VGIC 拥有投递以及客户机 enable/active 状态；
7. GICv2 EOI/DIR 或 GICv3 LR/TDIR 退休会在 controller lock 释放后到达 typed backend 退休边界；
8. host token 在 owner pCPU 上 deactivate。

拉低 timer line 不会完成 host activation。当客户机先清除 CVAL 或 CTL、再写 DIR 时，virtual line 可能已为低电平，但此前的投递在架构上仍是 active；因此这一点不可省略。

迁移可以在新 pCPU 装载 vCPU 前，强制完成旧 pCPU 上的 activation。Reset、stop 和 drop 同样会先使 timer-wheel 工作失效，再完成并丢弃 host activation。这些是显式生命周期操作，不可替代普通客户机退休路径。

## Assigned Physical SPI 生命周期

Assigned physical SPI 使用经过所有权校验的 HW-backed LR：

1. host top half acknowledge SPI 并执行 priority drop；
2. VGIC 排入一项权威投递，其中携带 guest INTID 与 physical INTID；
3. GICv2 把 `HW` 和 physical ID 写入 `GICH_LR`，GICv3 则把 `HW` 和 `PINTID` 写入 `ICH_LR_EL2`；
4. 普通客户机完成会让 GIC 硬件退休 physical activation，因此回收已经消失的 LR 时不得再次发出 host `DIR`；
5. 如果 level source 仍 asserted，physical deactivation 会重新采样并通过同一 ingress route 产生新的 host acknowledgement。

软件回收 stale LR snapshot 前，替代的 host acknowledgement 可能已经到达。Delivery de-duplication 必须保留该 acknowledgement，直到 refill 能创建新的 HW-backed LR；否则持续 asserted 的设备可能丢失唯一的替代 activation。硬件退休前发生的 trapped guest DIR，以及显式 rollback 或 teardown，会通过 typed backend 发出 host `DIR`。在 `DIR` 前采样 `GICD_ISPENDR` 不是等价的完成机制，因为 physical deactivation 才是架构定义的 level resample 点。

## WFI、Timer Wheel 与迁移

对于 WFI，`ArmTimerSnapshot::earliest_deadline` 同时考虑 CNTV 与 CNTP。Disabled、masked 或已经过期的 timer 不调度未来唤醒。

每个已调度 callback 携带：

- 一个由 `Aarch64TimerBinding` 分配的 WFI 等待代次；
- 一个包含 owner CPU 和 timer token 的 `VmTimerHandle`。

Callback 只校验 `Aarch64TimerBinding::wait_generation` 并唤醒 vCPU。在 task context 中，vCPU 重新读取当前 physical counter、发布 timer level，并在事件过早到达时重新 arm。它绝不只根据 timer-wheel callback assert PPI。

取消操作使用 handle 中记录的 owner CPU。远程取消在该 owner 上执行并重新编程它的 one-shot comparator，避免迁移后取消错误 CPU 的 queue，或积累长期 stale event。

当前 `ax-task` 的通用硬件 comparator 尚未分别记录 programmed、pending 和 active 状态。为避免在 timer IRQ 事件被处理前，重写一个已经过期的 comparator 并清掉待处理硬件中断，`begin_hardware_timer_irq` 只在进入匹配的 IRQ 路径后清除已编程 deadline 记录。该兼容保护可能让过期 comparator 在一段时间内继续作为调度参考，延迟后续 deadline 的重新编程；这是已接受的临时性能代价，不改变定时器正确性。只有当 IRQ acknowledge 路径能显式消费 comparator pending 状态、且不再依赖 comparator rewrite 时，才能删除该保护。

Reset、stop 和 drop 通过 `Aarch64TimerBinding::invalidate_wait` 推进等待代次。`ArmTimerContext` 只保存架构寄存器状态和 loaded 标志，不拥有调度代次；因此清空 timer context 不会让旧 callback 再次有效。

## 固件契约

存在 host FDT 时，AxVM 要求其中有有效的 `arm,armv8-timer` 节点，并解析：

- effective `interrupt-parent`；
- 恰好四项或五项 three-cell GIC PPI specifier；
- level trigger flag，包括保留的 legacy PPI CPU-mask bits；
- secure physical、non-secure physical、virtual、hypervisor 的固定中断顺序；
- 可选且非零的 `clock-frequency`；
- 可选的 timer phandle。

客户机 FDT 会删除已有 Arm timer 节点，并创建一个标准 `arm,armv8-timer` 节点。它保留中断顺序、parent identity、raw specifier flag、可选 phandle，以及显式有效的频率；不复制 host errata 或 suspend 属性。

Runtime vCPU 绑定和 FDT 安装校验并消费同一份 `GuestTimerProfile`；开发者提供的 DTB 不能引入独立的定时器资源定义。

没有 host FDT 时，固定 QEMU machine profile 提供标准四项 PPI。Host timer 节点一旦存在但 malformed 就返回错误，AxVM 不猜测板级资源。

## 失败与 Teardown

不支持或不一致的 timer capability 会使 VM 构建失败。实现不会：

- 继续使用当前 CPU 的频率或 IPA 位宽；
- 无限期 mask PPI 来替代 deferred deactivate；
- 把 timer PPI 转换为 assigned SPI；
- 根据板卡名称推断 IRQ；
- 在 reset 后保留 stale timer-wheel 工作；
- 保留第二个 timer device 或 relay 兼容路径。

注册是事务化的。重复注册 timer-PPI 失败时，不会 unregister 已有 binding。Teardown 在释放最终 binding 前移除 retirement route，并始终尝试完成自身拥有的 host activation。

## 验证

确定性回归覆盖：

- 清除 CNTVOFF 前禁用 CNTV 并执行 ISB；
- 完整 entry/exit 汇编操作顺序；
- 停止 CNTV 前完成 lower-EL IAR acknowledgement；
- CVAL/TVAL、ENABLE、IMASK、派生 ISTATUS、wraparound 和 timer context reset；
- 最早的 CNTV/CNTP WFI deadline；
- 目标 CPU 频率一致，以及目标 CPU capability 取最小值；
- owner-aware timer cancel 与 stale wait generation；
- GICv2 和 GICv3 hypervisor host CPU interface 都启用 split EOI；
- GICv2 EOI/DIR 与 GICv3 TDIR 退休；
- GICv2/GICv3 HW-backed assigned-SPI LR identity，以及普通硬件退休不重复 host deactivation；
- 替代 physical acknowledgement 能跨越 stale LR snapshot 保留；
- trapped DIR 总在 level resampling 前到达 physical deactivation；
- 未分配的 GICv3 LPI 能解析到 MSI leaf，并在 host `DIR` 中保留完整 24-bit INTID；
- EOI 后高电平重新 pending，低电平不重新 pending；
- host timer-PPI 只通过 VGIC retirement 完成；
- 四项/五项 FDT interrupts，以及 malformed cell、PPI class/trigger、frequency、parent、phandle 和 interrupt order。

合入前，验证矩阵还必须完成：

- QEMU AArch64 GICv2 与 GICv3 timer stress，覆盖 SMP、WFI 和重复 sleep；
- 现有 x86 VMX/SVM、RISC-V 与 Phytium smoke；
- RK3568 连续三次启动到达客户机 marker，且不发生 epoch jump；
- RK3588/OrangePi-5-Plus 重复通过，防止破坏既有路径；
- `arm_vcpu`、`arm_vgic` 与 `axvm` 定向 clippy 无新增 warning；
- 删除全部临时 timer/IRQ 诊断。
