# StarryOS 原生周期计数器采样

本文约束 [议题 #2673](https://github.com/rcore-os/tgoskits/issues/2673) 的 AArch64 内核修复，补充[现有 perf 设计](../docs/design/starry-aarch64-linux-perf.md)。内核应按已解析的事件选择可用 PMU 资源，让周期事件使用原生周期计数器采样；修复不修改 QEMU、容器或运行环境，也不能仅凭一次绿色运行推断间歇失败已经解决。

## 1. 采样语义

`perf::sys_perf_event_open()` 创建逻辑事件，owner CPU 的 PMU 与 `sampling::SampleSlot` 执行计数和采样。硬件事件可以只计 EL0，但溢出 IRQ 的交付现场仍可能位于 EL1；计数过滤并不保证中断现场的特权级。

### 1.1 真实中断现场

`ax_hal::irq::interrupted_context()` 只表示本次 IRQ 动态作用域内的真实 `InterruptedContext`。`pmu_overflow_handler()` 必须从这份值快照取得 PC 和 privilege，`PERF_RECORD_MISC_USER/KERNEL` 不能因事件配置而改标。用户 SP、FP 和 LR 必须来自真实保存的用户寄存器，不能从内核寄存器或猜测地址构造。

现有 `unwind::user_callchain()` 在 no-fault 用户内存边界内回溯，`build_sample()` 按请求字段编码记录。没有真实用户现场时，不能通过伪造 `PERF_CONTEXT_USER`、LR 或 ABI_64 满足测试。

### 1.2 兼容边界

Linux 基准固定为 v7.1，提交 `8cd9520d35a6c38db6567e97dd93b1f11f185dc6`。[`sample_is_allowed()`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/kernel/events/core.c#L10609-L10620) 在 `exclude_kernel` 下拒绝实际现场为内核态的样本，包括 user-only 计数器的 IRQ 延迟交付。Starry 当前缺少这层发布过滤，这是独立的兼容性缺口，本次不通过丢掉 EL1 样本冒充用户链修复，也不宣称已经补齐该语义。

本次保持现有真实现场和 no-fault 回溯边界，不引入保存用户上下文的新接口、补偿性软件采样或 PMU 轮询。后续补齐采样许可时仍须正常结算被过滤的溢出、续接周期，不能产生虚假 LOST 或使计数器停滞。

## 2. 资源与周期

Linux 的 [`armv8pmu_can_use_pmccntr()` 与 `armv8pmu_get_event_idx()`](https://github.com/torvalds/linux/blob/8cd9520d35a6c38db6567e97dd93b1f11f185dc6/drivers/perf/arm_pmuv3.c#L981-L1054) 按归一化事件编码选择原生周期计数器。`hw_open::validate_perf_event_open_hw()` 应采用相同的事件归一化边界，不因 RAW、generic、named 或 sampling 的表达方式排除 `CPU_CYCLES`。该规则仅适用于已支持的属性，不新增 threshold、branch-stack 或其他未实现能力。

### 2.1 分配与回收

`hw_allocation` 继续是原生周期计数器和普通槽的唯一分配所有者。task event 优先复用已有 task-cycle 保留与冲突回退；CPU sampling 优先取得目标 CPU 的原生计数器，冲突时保留一个真实普通槽，不能退入只有计数功能的 flexible 占位路径。现有 task 全局 cycle 保留策略保持不变，不扩大为新的每核调度分配器。

`Counter`、`PmuRunLease` 和 `SampleRegistration` 应携带足以识别原生周期计数器的身份，`hw_owner` 集中转换到已有 `CounterId::CYCLE`。registry 支持 bit 31，IRQ、读数和停表均使用同一硬件身份。禁止在多个调用点分散猜测裸索引含义；登记代际、owner CPU 和 backing anchor 的寿命约束不变。

### 2.2 位宽与中断

`SamplingCount` 按计数器活动宽度结算 raw delta：普通计数器保留 32 位模运算，原生周期计数器保留完整 64 位值和负周期 preload。逻辑采样周期仍受现有 u32 策略约束，`period_left`、中断 overshoot、跨切片续接以及 group read 不应因换用硬件计数器而改变。

累计值采用现有 u64 饱和记账。剩余周期的有符号算术域至少覆盖 i128，扣减整个 u64 delta 时保留精确的负超期量，不得先以 `as i64` 造成符号翻转，也不得将结果向零饱和。重装沿用 `armpmu_event_set_period()` 的规则：超期达到一个完整逻辑周期时从完整周期重新开始，否则把负超期量加回周期，续接尚未完成的部分。软件周期限制不改变计数器活动宽度，32 位负 preload 和 64 位负 preload 不能混用。

原生周期采样仍需可用的 PMU IRQ。准入失败必须在发布 worker、registry 和启用硬件前完成，已保留资源回滚；无 IRQ 环境仅保留原有非继承纯计数能力。disable、RESET、切出和 close 先停表并结算，再撤销准确的一代登记，最后释放槽与引用；后续重新启用或打开新事件不能继承旧 pending 状态。

统一 RAW 周期选择后，原本使用 flexible 控制路径的纯计数事件也可能进入原生周期路径。其有限寄存器事务必须与现有 flexible 控制保持相同的进展保证：本核直接执行，远端 read、启停和 RESET 通过同步 owner-CPU rendezvous 完成，不能等待会被 FIFO 负载饿死的普通 worker。涉及采样登记或输出引用迁移的操作仍须遵守任务上下文准备与回收边界；不得把最后一个 `Arc` 的析构、分配或可睡眠操作移入 IPI。范围内的纯计数控制应复用已有 `run_on_cpu_sync()`，不新增调度器、忙轮询或后台控制队列。

## 3. 回归边界

`perf-hw-callchain-user` 是真实 PMU、IRQ、用户帧链与 mmap ring 的系统级功能证明。修复应增强它的诊断和可控边界，而不是用宿主伪实现代替真实采样。

### 3.1 可观察判据

同一用例继续要求至少一个 sample、至少四个真实用户 IP、有效用户 LR，以及完整且身份匹配的 record。`perf_hw_callchain_user.c` 的失败必须经 grouped runner 传播到最外层 `cargo xtask`；不得增加循环次数、超时、重试或放宽成功匹配来掩盖故障。

诊断应能区分事件是否累计到周期、IRQ 在 EL0 还是 EL1 交付，以及 record 是否完整。必要信息包括 sample 的 misc、IP、time、callchain、用户寄存器 ABI 和事件计数；诊断有界，临时探针不成为长期 ABI。

### 3.2 失败证明

回归必须控制真实实现的资源边界：普通槽被占用而原生周期计数器仍空闲时，周期采样仍须产生合格用户链。旧内核不能使用该空闲资源，修复后应从实际原生计数器产生样本；若以内核集成用例观察资源身份，还须以原系统用例证明真实用户链。专用计数器被占用时的普通槽回退、task/CPU 启停、RESET、分组读取与关闭后复用也需有直接证据。概率性重复运行只能提供调查线索，不构成确定性红绿证明。

受控资源场景复用现有 Starry perf 用例：在固定目标 CPU 上，以非周期 RAW 事件创建 disabled 的 CPU sampling fd，利用其 open-time 保留契约占满普通槽；在架构容量上界内首次得到 `EBUSY` 才说明压力条件成立，其他错误必须失败并清理。随后创建周期采样并执行原四层用户负载；普通槽全部被保留时仍能输出样本，是使用独立原生资源的可观察证明，不要求开放用户直接 PMU 读取权限。冲突回退以先持有周期资源、再成功取得普通槽或在普通槽全满时明确 `EBUSY` 为判据，关闭并重开验证回收。若用户态不能构造完整边界，采用 `perf` 生产模块已有 kernel axtest 入口，在真实 owner CPU 上观察 lease 的计数器身份及实际 overflow，再由原用户用例证明四帧和 LR。

本次验收首先使用议题原命令 `AXBUILD_NO_SNAPSHOT=1 cargo xtask starry test qemu --arch aarch64 --test-case qemu/system/perf-hw-callchain-user`，再按实际影响选择相邻采样、周期和 ring 回归。QEMU 的 icount 结果只证明该环境中的接口与交付行为，不代替实体 PMU 性能或板卡验证。

使用当前已安装的 QEMU 和原 profile，不修改或替换运行环境。原负载保持 `400000` 次、`sample_period=100000`，超时及成功匹配不变；资源准备不能通过增加用户负载、主动反复读 PMU 或插入补偿性系统调用制造样本。

算术边界由 `SamplingCount` 所属模块的既有测试证明 32/64 位模差、超过 u32 的真实计数范围及部分周期超期续接；相邻 period 用例证明真实切片续接，原用户用例证明 PMCCNTR overflow、四帧和 LR。各层证据不得互相冒充，不因补边界测试改变原用户负载。

Starry 通过 `include!("root.rs")` 关联生产模块，常规 `cargo fmt` 不一定遍历其中的源文件，格式验收还须由固定工具链的 Rustfmt 直接覆盖实际变更文件。`axtest` feature 的 Clippy 检查不等于启用 `cfg(test)`；上述模块内测试必须由 `cargo xtask ktest qemu -p starry-kernel --arch aarch64 --config test-suit/starryos/qemu/build-aarch64-unknown-none-softfloat.toml` 实际编译和执行。
