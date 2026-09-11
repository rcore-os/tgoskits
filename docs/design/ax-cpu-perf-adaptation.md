# ax-cpu 与 perf 消费接口迁移

本文对照 PR #2274 的精确提交 `535c98535bb241e85c7181e85aaa16c3e2d25577`，记录 CPU 重构后的接口对应关系。该提交在 2026-09-10 核对时仍未合入 dev。本次不整体引入该 PR 的事件组、复用、sysfs 或 Linux ABI 功能，也不修改其远端分支。

## 1. CPU 契约

寄存器行为以本地 Linux v7.1 提交 `8cd9520d35a6c38db6567e97dd93b1f11f185dc6` 的 `drivers/perf/arm_pmuv3.c`、`arch/arm64/include/asm/arm_pmuv3.h` 和 `include/linux/perf/arm_pmuv3.h` 为核对来源。PR 的需求决定需要哪些能力，不能替代硬件语义核对。

### 1.1 本核访问

`unsafe Pmu::current()` 建立保持本核不变、排除 IRQ 重入与其他 PMU 所有者的会话。CPU 不接收 `CpuPin`，也不取得运行期锁。Starry 的 `hw_owner::on_pmu` 在 IRQ/抢占守卫和本核 pin 下提供这层运行期保证，并在首次访问本核 PMU 时完成一次独占初始化；回调只能执行有界寄存器操作。HAL 的 `with_current` 仍供其他拥有明确计数器所有权的运行期消费者使用。

| 精确 head 中的入口 | 重构后 CPU 入口 | 消费者调整 |
| --- | --- | --- |
| `pmu::probe()` | `Pmu::current()?.info()` | 在 owner CPU 保存能力，不在调用 CPU 猜测其他核的宽度 |
| `pmu::init_cpu()` | 独占初始化时 `Pmu::reset()`，随后按需 `start()` | `perf/percpu.rs` 先取得本核整体所有权，不能重复重置活跃事件 |
| `cycles::{configure,enable,disable,read,reset}` | `CounterId::CYCLE` 配合会话方法；单计数器清零为 `write(id, 0)` | 保留专用计数器选择，不与可编程槽混用 |
| `counter::{configure,enable,disable,read,write,preload}` | `Pmu::counter(n)?` 后调用相应方法 | 索引错误返回失败，不能读零或忽略写入 |
| `overflow::{status,clear,enable_irq,disable_irq}` | `overflow_status`、`clear_overflow`、`enable_overflow_irq`、`disable_overflow_irq` | 状态掩码采用 `u64`，不得截掉 PMICNTR 的位 32 |
| `with_counters_paused(pin, callback)` | `unsafe Pmu::with_counting_paused(callback)` | CPU 固定范围由上层保持；回调接收同一 `&mut Pmu`，不得再建立重叠会话 |

暂停操作保留每个计数器的启用位、值与溢出状态，返回或展开时恢复进入前的全局启停状态。`reset` 与 `start` 不等价；将普通事件启用改成全局重置会破坏其他槽。

### 1.2 身份与事件证据

CPU 的 `PmuInfo` 保存 PMU 版本、数量、有效宽度、完整 PMCEID 和独立的 PMICNTR 能力。`capability::Midr` 保存机器身份字段，平台编号与 cluster 分类不进入 CPU。

| 精确 head 中的内容 | 新所有者及差异 |
| --- | --- |
| `PmuInfo.midr`、`read_midr_el1()` | `capability::Midr::read()`；Starry 的本核记录同时保存 identity 与 PMU info |
| `ClusterId`、`classify_midr`、`cluster_id` | 留在 Starry 的 CPU 分组策略，基于实际 MIDR，不基于逻辑编号 |
| `PmuInfo::event_supported` 的布尔结果 | `event_support` 返回 `Supported`、`Unsupported`、`ImplementationDefined`；位图范围外不能宣称已获硬件支持 |
| `hw_event_to_arm_with`、`hw_cache_to_arm`、`CacheEventError` | Linux hardware/HW_CACHE 编码与 fallback 留在 Starry；当前 dev 的 hardware 映射位于 `perf/event_map.rs` |
| 固定 32 位 programmable counter | 读取 owner 保存的有效宽度；若采用 CPU reset 后的长计数模式，sampling 的差值、preload、回绕与 mmap 宽度必须同步 |

PMICNTR 按 `ID_AA64DFR1_EL1.PMICNTR` 独立探测，选择 `CounterId::INSTRUCTIONS`，不从 PMUVer 推断。PMCEID 同时处理基本和扩展公共事件位图。CPU 不接受 Linux 的事件编号作为硬件事件编码。

## 2. 消费者差异

以下差异用于适配上述精确 head，不能把当前 dev 的整个 perf 目录覆盖到该分支。该 PR 的 `percpu`、`system_flex`、`CountingState` 和组采样有独立状态，迁移时保留这些上层所有者。

### 2.1 IRQ 现场

`sampling::pmu_overflow_handler` 不再读取 PMU 模块的全局现场或重新读取 ELR/SP。类型从 `ax_cpu::trap::{InterruptedContext, InterruptedPrivilege}` 导入，动态作用域从 `ax_hal::irq::interrupted_context()` 获取。缺少现场时由上层明确选择不采集 callchain，不能伪造用户 PC/SP。

接口差异可直接按下面的调用关系核对：

```diff
- let interrupted = ax_cpu::pmu::interrupted_context();
- let ovf = ax_cpu::pmu::overflow::status();
- ax_cpu::pmu::overflow::clear(ovf);
+ let interrupted = ax_hal::irq::interrupted_context();
+ let ovf = pmu.overflow_status();
+ pmu.clear_overflow(ovf);

- ax_cpu::pmu::with_counters_paused(pin, || service_overflowed_slots(...))
+ unsafe { pmu.with_counting_paused(|pmu| service_overflowed_slots(pmu, ...)) }
```

片段中的 `pmu` 是 IRQ 所有者已经建立的同一个会话。`service_overflowed_slots` 及读组成员的路径需传递该借用，而不是在暂停回调内部再次进入 `hw_owner::on_pmu`。slot registry、generation 锁、累计回绕、采样重装与 callback 强引用继续由 perf 维护。

### 2.2 用户直接读取

CPU 提供 `enable_user_access(readable_mask)` 和 `disable_user_access()`，不代替 perf 的授权决策。PMUv3p9 使用 PMUACR；较早版本无法逐计数器限制 EL0，授权前必须保证其余计数器已经停止且无所有者，随后清零未授权内容。

`perf/rdpmc.rs` 发布 `cap_user_rdpmc`、index 和 pmc_width 前，必须取得 owner CPU 上已成功授权的事实。任务切出、事件停止、槽转让与销毁先撤权，再发布不可直接读取状态。不得仅因建立 mmap 就宣称可直接读取，也不得从执行 mmap 的 CPU 获取远端槽的宽度。未完成上述所有权协议的消费者应保持能力位关闭并使用普通 read 路径；CPU 层不能通过全局授权替上层掩盖缺口。

## 3. 验证证据与范围

验证分开记录 CPU 机制和 perf 所有者语义。PR 的历史 ABI 或板卡结果不能代替本次 CPU 改动的运行证据。

### 3.1 CPU 集成用例

`test-suit/arceos/cpu/pmu` 通过 ArceOS 运行期在 AArch64 A53 与 `max` QEMU 上验证计数、预装载、溢出状态、非法索引、授权清零和暂停恢复。`paging-el2` 与 `guest-entry` 分别核对显式 EL2 翻译和客户机退出后的宿主恢复。CPU crate 不包含测试或伪寄存器。

### 3.2 仍需真实环境的证据

当前 QEMU 未暴露 PMICNTR，专用 instruction counter 与 PMUv3p9 的权限路径需相应硬件运行。PMU IRQ 投递、真实 EL0 读取与撤权后的陷入、用户态溢出现场已经通过 ArceOS QEMU 及 OrangePi 5 Plus 八核验证，逐核 MIDR 与会话见 [验证记录](ax-cpu-validation.md)。这些是 CPU 机制证据，不替代 Starry perf 的事件授权、任务切换和槽位转让协议。当前消费者在没有该授权协议时保持 mmap 直接读取能力关闭。

### 3.3 最新 PR 增量

`535c98535b` 相对前次核验的 `e5b17fb240` 增加 owner CPU 独立槽位、软件事件和 sideband 等策略修正，没有新增 CPU 寄存器后端。适配时保留 `hw_allocation::alloc_system` 的本核所有权；其中事件支持判断改为 owner 保存的 `PmuInfo::event_support`，不能恢复到调用 CPU 的全局 probe。该增量不改变本文的 PMU、现场和用户访问接口。
