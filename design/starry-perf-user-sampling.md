# StarryOS 用户调用链采样边界

本文约束 [议题 #2673](https://github.com/rcore-os/tgoskits/issues/2673) 的 AArch64 perf 修复，补充[现有 perf 设计](../docs/design/starry-aarch64-linux-perf.md)。修复必须区分硬件计数来源、溢出中断现场和输出记录的用户上下文，不能仅凭一次绿色运行推断间歇失败已经解决。

## 1. 采样语义

`perf::sys_perf_event_open()` 创建逻辑事件，owner CPU 的 PMU 与 `sampling::SampleSlot` 执行计数和采样。硬件事件可以只计 EL0，但溢出 IRQ 的交付现场仍可能位于 EL1；计数过滤并不保证中断现场的特权级。

### 1.1 真实中断现场

`ax_hal::irq::interrupted_context()` 只表示本次 IRQ 动态作用域内的真实 `InterruptedContext`。`pmu_overflow_handler()` 必须从这份值快照取得 PC 和 privilege，`PERF_RECORD_MISC_USER/KERNEL` 不能因事件配置而改标。用户 SP、FP 和 LR 必须来自真实保存的用户寄存器，不能从内核寄存器或猜测地址构造。

现有 `unwind::user_callchain()` 在 no-fault 用户内存边界内回溯，`build_sample()` 按请求字段编码记录。没有真实用户现场时，不能通过伪造 `PERF_CONTEXT_USER`、LR 或 ABI_64 满足测试。

### 1.2 采样许可

Linux 基准固定为 v7.1，提交 `8cd9520d35a6c38db6567e97dd93b1f11f185dc6`。[`sample_is_allowed()`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/events/core.c#L10609-L10620) 对实际中断现场应用 `exclude_kernel/exclude_user`，包括 user-only 事件的 IRQ 延迟到内核交付的情况。Starry 的采样许可也应由事件策略和实际现场共同决定，而不是仅依赖 PMU 的计数过滤。

被过滤的溢出仍须完成计数结算、周期续接和必要的重装，不能使计数器停滞或产生虚假 LOST。ring、callchain 和寄存器记录只为允许输出的样本生成。此契约不引入新的保存用户上下文接口，也不宣称补足所有 perf 能力。

## 2. 回归边界

`perf-hw-callchain-user` 是真实 PMU、IRQ、用户帧链与 mmap ring 的系统级功能证明。修复应增强它的诊断和可控边界，而不是用宿主伪实现代替真实采样。

### 2.1 可观察判据

同一用例继续要求至少一个 sample、至少四个真实用户 IP、有效用户 LR，以及完整且身份匹配的 record。`perf_hw_callchain_user.c` 的失败必须经 grouped runner 传播到最外层 `cargo xtask`；不得增加循环次数、超时、重试或放宽成功匹配来掩盖故障。

诊断应能区分事件是否累计到周期、IRQ 在 EL0 还是 EL1 交付，以及 record 是否完整。必要信息包括 sample 的 misc、IP、time、callchain、用户寄存器 ABI 和事件计数；诊断有界，临时探针不成为长期 ABI。

### 2.2 失败证明

回归必须控制真实实现的关键采样边界，使同一判据在旧逻辑上因预期原因失败、修复后通过。概率性重复运行只能提供调查线索，不构成确定性红绿证明。若证据要求改变跨层上下文所有权，应先确认并更新设计，不能在 perf 层隐式补造现场。

本次验收首先使用议题原命令 `AXBUILD_NO_SNAPSHOT=1 cargo xtask starry test qemu --arch aarch64 --test-case qemu/system/perf-hw-callchain-user`，再按实际影响选择相邻采样、周期和 ring 回归。QEMU 的 icount 结果只证明该环境中的接口与交付行为，不代替实体 PMU 性能或板卡验证。
