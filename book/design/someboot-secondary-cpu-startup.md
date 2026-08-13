# someboot secondary CPU 启动握手

## 状态

本文定义 someboot 启动 secondary CPU 时的平台层同步契约。它覆盖四种架构的共同生命周期、架构 transport 边界、per-CPU 存储、非阻塞查询接口，以及它与上层超时策略和 OS scheduler online 状态的关系。修改这些边界时必须同步更新本文和 `arch-platform-porting` skill。

## 问题与成功标准

旧 x86 路径用一个全局 `AP_BOOTED_ID` 和私有启动锁确认 AP 到达架构入口，并在 500 ms 后超时。其他架构则只以 PSCI、SBI 或 mailbox 调用返回作为启动完成。统一 per-CPU 握手后，同步 `cpu_on()` 又把 prepare、transport、查询、10 秒超时和 release 固定在一次阻塞调用中，上层既不能轮询，也不能接入未来的真实异步唤醒源。

本设计的成功标准是：

- 每个 logical CPU 都有独立的 `DEAD -> KICKED -> ALIVE -> SHOULD_ONLINE` 状态；
- BSP 只能释放实际报告 `ALIVE` 的目标 CPU；
- 四架构 hook 只发送 PSCI、SBI、mailbox 或 INIT/SIPI 请求；
- secondary 在进入 OS runtime 前报告 `ALIVE`，并在 BSP 明确释放前等待；
- someboot 的公共接口只执行 `start/status/release`，不在启动请求内等待 AP；
- 同一时间只有一个不可复制的 typed handle 拥有共享启动 transport；
- `axplat-dyn` 保持同步 `PowerIf` 契约，在 10 秒内轮询并返回可匹配的 `CpuOnError::AliveTimeout`；
- `PerCpuMeta` 继续保持不可变 trampoline ABI。

非目标包括 CPU hotplug、someboot `Future`、启动 cancel/retry、fallback transport、scheduler online 发布、timer/clockevent 生命周期重构，以及 secondary 进入 OS 后的失败回滚。

## Prior art

设计对照本地 Linux v7.1 源码 commit `8cd9520d35a6c38db6567e97dd93b1f11f185dc6`：

- `arch/x86/include/asm/apicdef.h` 将 `APIC_DM_STARTUP` 定义为 `0x00600`；SIPI 是 edge-triggered delivery，不携带 INIT 的 level-assert 位。
- `kernel/cpu.c` 的 `cpu_up()` 对调用方仍表现为同步事务，但内部将 architecture kick、AP completion 和 CPU hotplug 状态推进分成不同阶段。
- `arch/x86/kernel/smpboot.c` 将 INIT/SIPI 发送和 AP 后续启动阶段分开处理。
- `arch/riscv/kernel/smpboot.c` 同样把 SBI hart start transport 与 generic secondary completion 分开。
- `include/linux/cpuhotplug.h` 将 CPU bring-up 的 starting 阶段与最终 online 生命周期分开。

TGOSKits 不复制 Linux hotplug 状态机，也不照搬 Linux 的公开同步接口。这里只复用内部阶段化的成熟边界：架构代码负责 wake transport；平台到达启动同步点与 scheduler online 是不同事实；等待策略属于具有时间源的上层。

## 所有权与存储

someboot 是启动握手的唯一 owner。`CpuBootSync` 由 someboot 在每个 CPU area 中单独分配并原位构造，地址在关机前保持稳定。`CPU_AREA_RUNTIME_COUNT` 的 Release 发布发生在全部 `CpuBootSync` 和 `PerCpuMeta` 构造及 cache maintenance 完成之后；读取方通过 Acquire 观察发布。

`CpuBootSync` 不放进 `PerCpuMeta`。后者由 trampoline 和架构入口按值读取，发布后只包含 stack、hardware ID、logical index、entry 和 page-table 地址。将原子状态塞进该结构会把不可变启动 ABI 与运行时可变同步混在一起。

## 状态机

每个 CPU 的状态只能按以下顺序推进：

```text
DEAD --BSP prepare--> KICKED --secondary report--> ALIVE --BSP release--> SHOULD_ONLINE
```

- `prepare_secondary_boot(cpu)` 以 AcqRel CAS 将该 CPU 从 `DEAD` 改为 `KICKED`。
- secondary 到达公共 someboot entry 后，以 AcqRel CAS 将自身从 `KICKED` 改为 `ALIVE`。
- BSP 以 Acquire 查询目标 CPU；查询 `ALIVE` 不执行任何状态转换。
- BSP 只对目标 CPU 执行 `ALIVE -> SHOULD_ONLINE` CAS；其他 CPU 报告 alive 不会满足该 release。
- secondary 以 Acquire 读取 `SHOULD_ONLINE`，观察到后才调用 `__someboot_secondary`。
- 重复 prepare、未 kick 就报告 alive、重复 release 都是状态不变量错误，不隐式 retry 或重置状态。

## 公共接口与启动顺序

someboot 用不可复制、不可构造且带 `#[must_use]` 的 `SecondaryCpuStartup` 表示唯一 in-flight 启动：

```rust
pub fn start_secondary_cpu(
    logical_cpu: usize,
) -> Result<SecondaryCpuStartup, CpuOnError>;

pub enum SecondaryCpuStartupStatus {
    WaitingForAlive,
    Alive,
}

impl SecondaryCpuStartup {
    pub fn logical_cpu(&self) -> usize;
    pub fn hardware_id(&self) -> usize;
    pub fn status(&self) -> SecondaryCpuStartupStatus;
    pub fn release(self) -> Result<(), CpuOnError>;
}
```

`start_secondary_cpu()` 对目标 CPU 依次执行：

1. 以 CAS 获取全局 `ACTIVE_SECONDARY_CPU`；已有启动时立即返回 `StartupInProgress`，不等待锁。
2. 解析 logical CPU 对应的 immutable `PerCpuMeta`；无效目标在 transport 开始前安全归还 owner。
3. 清理启动映像所需 cache，并将该 CPU 标记为 `KICKED`。
4. 调用 `ArchTrait::kick_secondary_cpu` 发送架构 transport。
5. 返回拥有 logical CPU 和 hardware ID 的 handle，不等待 `ALIVE`。

handle 的操作严格分离观察和修改：

- `status()` 只以 Acquire 读取目标 `CpuBootSync`，将 `KICKED` 映射为 `WaitingForAlive`、`ALIVE` 映射为 `Alive`，没有 CAS 或隐式 release。
- `release(self)` 只在目标为 `ALIVE` 时执行 `ALIVE -> SHOULD_ONLINE` CAS；CAS 成功后才清除 `ACTIVE_SECONDARY_CPU`。consume handle 防止同一次启动被重复释放。
- 非 `ALIVE` release 返回 `CpuOnError::NotAlive`，不会清除 owner。

`axplat-dyn::PowerIf::cpu_boot()` 保持已有同步接口，在具有稳定 somehal timer 的层次执行：

1. 调用 `start_secondary_cpu()`；
2. 轮询 `status()`，看到 `Alive` 后调用 `release()`；
3. 10 秒内未到达 `Alive` 则返回带 logical CPU 和 hardware ID 的 `AliveTimeout`。

因此“非阻塞”只表示 someboot 不等待 AP `ALIVE`。x86 INIT/SIPI 规范要求的短延迟和 IPI delivery 检查仍是一次 transport 调用的组成部分，不能移到无状态查询中。

secondary 的公共路径先完成架构 `per_cpu_trap_init(false)` 并解析自身 metadata，然后报告 `ALIVE`、等待 release，最后调用 `__someboot_secondary`。因此 `ALIVE` 证明目标 CPU 已到达 final stack/page-table/common-entry 边界；它不证明 scheduler、runtime IRQ、task queue 或其他 OS per-CPU 服务已经 online。

## 串行化与放弃语义

四架构共用一致的单 in-flight 契约。即使 PSCI 或 SBI 平台可能支持并行启动，x86 共享 `0x8000` trampoline 和 boot parameters 在整个 prepare/kick/alive/release 生命周期中都不能被下一个目标覆盖；公共接口不暴露依赖架构的并发差异。

以下情况都不会清除 `ACTIVE_SECONDARY_CPU`：

- 未完成 handle 被丢弃；
- 包装 handle 的异步任务被取消；
- 上层等待超时；
- architecture transport 返回可能发生在部分发送之后的错误；
- 在目标尚未 `ALIVE` 时错误调用 `release()`。

这些情况都不能证明目标 CPU 没有开始执行。开放下一个 trampoline 会让迟到 AP 读取被覆盖的 metadata，因此本次启动被视为本次 boot 的终止性失败，不提供 cancel、retry 或 fallback。只有在 architecture transport 开始前的 metadata/状态 prepare 失败，才能安全回收 owner。

## 为什么 someboot 不提供 `Future`

someboot 运行在 executor、task runtime 和可靠 timer/waker 建立之前。把轮询简单包装成 `Future` 不会产生真实唤醒源，只会把 busy polling 隐藏成伪异步，并让 cancellation 语义不明确。

未来具有 timer 和硬件事件唤醒能力的上层可以基于 `SecondaryCpuStartup` 自行包装异步等待，但必须同时满足：

- `ALIVE` 状态变化能触发真实 waker；
- cancellation 被当作终止性启动失败，不能隐式释放或复用 transport；
- 最终仍由唯一 handle 显式执行 `release()`。

## 架构边界

`ArchTrait::kick_secondary_cpu` 只负责 transport：

- AArch64：PSCI `CPU_ON`；
- RISC-V：SBI HSM `hart_start`；
- LoongArch：boot argument mailbox 和 boot IPI；
- x86_64：共享 trampoline 准备、INIT assert/deassert 和两次 SIPI。

x86 SIPI 使用 `0x600 | vector`。x86 不再维护 `AP_BOOTED_ID`、架构私有启动锁或 500 ms AP 轮询。通用 `ACTIVE_SECONDARY_CPU` 以非阻塞 CAS 串行化完整 prepare/kick/alive/release 生命周期，也保护 x86 共享 trampoline 在前一个 AP 被显式 release 前不被覆盖。

## 平台 alive 与 scheduler online

这两个状态源表达不同事实，不能合并：

- someboot `ALIVE/SHOULD_ONLINE` 只属于启动 handoff，回答“该 CPU 是否到达平台同步点，以及 BSP 是否允许它进入 OS”。
- OS/runtime scheduler online 回答“该 CPU 的 scheduler、IRQ、timer 和运行时服务是否已经完成发布，可否接收普通工作”。

someboot 不读取 scheduler online 来完成启动握手，OS 也不把 `SHOULD_ONLINE` 当作最终 online publication。`axruntime` 的 `ENTERED_CPUS` 等待继续保留：它证明 CPU 已进入 OS runtime，而不是重复 someboot 的 platform-alive 状态。这样避免循环依赖：OS online 初始化必须先获得 someboot release 才能执行，而 someboot release 不能反过来等待 OS online。

## 失败与兼容性

`CpuOnError::StartupInProgress` 同时携带 requested 和 active logical CPU；`NotAlive` 与 `AliveTimeout` 携带 logical CPU 和 hardware ID。超时上限固定为 10 秒，但策略由 `axplat-dyn` 执行；someboot 只保留可供上层匹配的 typed error。任何失败都不执行 retry、备用 transport 或假成功。transport 自身错误继续由架构 hook 映射为 `NotSupported`、`AlreadyOn`、`InvalidParameters` 或带上下文的 `Other`。

这是 someboot/somehal 公共 Rust API 的 breaking change：同步 `cpu_on()` 被 `start_secondary_cpu()` 和 typed handle 替代。`ax-plat::PowerIf` 保持同步，唯一动态平台调用方在同一变更中迁移。`PerCpuMeta` 的布局和含义保持不变。

## 方案比较

| 方案 | 结论 |
| --- | --- |
| 保留各架构私有完成信号 | 拒绝；四架构对“启动完成”的语义继续不同，x86 全局状态仍是额外 owner。 |
| 把状态加入 `PerCpuMeta` | 拒绝；会混合 immutable trampoline ABI 与 mutable synchronization。 |
| 等待 OS scheduler online | 拒绝；会让平台 release 与 OS 初始化形成循环依赖。 |
| someboot 直接返回 `Future` | 拒绝；早期启动没有 executor/waker，无法提供真实异步进展和安全 cancellation。 |
| someboot 同步等待 10 秒 | 拒绝；把查询机制、时间策略和阻塞行为绑定在最低层，无法供轮询或未来异步上层复用。 |
| 超时后 retry 或 fallback | 拒绝；可能对已运行但尚未报告的 CPU 重复 kick，并掩盖真实 bring-up 失败。 |
| 非阻塞 typed handle + 独立 `CpuBootSync` | 采用；状态按 CPU 隔离，查询无副作用，transport 和等待策略边界清楚，并可在最低层确定性测试。 |

## 验证

- x86 单元测试验证 SIPI base 为 `0x600` 且不含 level-assert 位；旧 `0x4600` 必然失败。
- 状态机单元测试验证查询 `ALIVE` 不会 release，只有报告 `ALIVE` 的同一 `CpuBootSync` 可以 release，并覆盖非法顺序和未发布 CPU。
- owner 单元测试验证第二次 start 立即失败、错误 CPU 不能释放 owner、只有显式匹配 release 后下一个 CPU 才能获取 transport。
- `cargo test -p someboot` 覆盖状态、layout 和现有启动契约。
- `cargo xtask clippy --package someboot --package axplat-dyn` 覆盖修改 crate 的 feature 组合。
- 四架构 ArceOS `task-smp-online` QEMU 用例验证实际 secondary handoff，以及 someboot alive 与 runtime entered 两个状态源都能推进。
