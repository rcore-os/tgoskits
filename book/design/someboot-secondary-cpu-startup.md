# someboot secondary CPU 启动握手

## 状态

本文定义 someboot 启动 secondary CPU 时的平台层同步契约。它覆盖四种架构的共同生命周期、架构 transport 边界、per-CPU 存储、超时语义，以及它与 OS scheduler online 状态的关系。修改这些边界时必须同步更新本文和 `arch-platform-porting` skill。

## 问题与成功标准

旧 x86 路径用一个全局 `AP_BOOTED_ID` 和私有启动锁确认 AP 到达架构入口，并在 500 ms 后超时。其他架构则只以 PSCI、SBI 或 mailbox 调用返回作为启动完成。这样产生了两个问题：

1. “已发送硬件唤醒请求”和“目标 CPU 已进入可继续执行的 someboot 公共入口”没有统一状态语义。
2. 全局 last-CPU ID 不能独立表达每个 logical CPU 的启动状态，并把 x86 transport 私有事实误当成通用启动完成。

本设计的成功标准是：

- 每个 logical CPU 都有独立的 `DEAD -> KICKED -> ALIVE -> SHOULD_ONLINE` 状态；
- BSP 只能释放实际报告 `ALIVE` 的目标 CPU；
- 四架构 hook 只发送 PSCI、SBI、mailbox 或 INIT/SIPI 请求；
- secondary 在进入 OS runtime 前报告 `ALIVE`，并在 BSP 明确释放前等待；
- 10 秒内未到达同步点时返回可匹配的 `CpuOnError::AliveTimeout`；
- `PerCpuMeta` 继续保持不可变 trampoline ABI。

非目标包括 CPU hotplug、启动 retry、fallback transport、scheduler online 发布、timer/clockevent 生命周期重构，以及 secondary 进入 OS 后的失败回滚。

## Prior art

设计对照本地 Linux v7.1 源码 commit `8cd9520d35a6c38db6567e97dd93b1f11f185dc6`：

- `arch/x86/include/asm/apicdef.h` 将 `APIC_DM_STARTUP` 定义为 `0x00600`；SIPI 是 edge-triggered delivery，不携带 INIT 的 level-assert 位。
- `arch/x86/kernel/smpboot.c` 将 INIT/SIPI 发送和 AP 后续启动阶段分开处理。
- `include/linux/cpuhotplug.h` 将 CPU bring-up 的 starting 阶段与最终 online 生命周期分开。

TGOSKits 不复制 Linux hotplug 状态机。这里只复用两个成熟边界：架构代码负责 wake transport；平台到达启动同步点与 scheduler online 是不同事实。

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
- BSP 只对目标 CPU 执行 `ALIVE -> SHOULD_ONLINE` CAS；其他 CPU 报告 alive 不会满足该等待。
- secondary 以 Acquire 读取 `SHOULD_ONLINE`，观察到后才调用 `__someboot_secondary`。
- 重复 prepare、未 kick 就报告 alive、重复 release 都是状态不变量错误，不隐式 retry 或重置状态。

## 启动顺序

BSP 对 secondary CPU 逐个执行：

1. 解析 logical CPU 对应的 immutable `PerCpuMeta`。
2. 清理启动映像所需 cache，并将该 CPU 标记为 `KICKED`。
3. 调用 `ArchTrait::kick_secondary_cpu` 发送架构 transport。
4. 轮询目标 CPU 的 `ALIVE`，最多等待 10 秒。
5. CAS 为 `SHOULD_ONLINE`，允许该 CPU 进入 OS runtime。

secondary 的公共路径先完成架构 `per_cpu_trap_init(false)` 并解析自身 metadata，然后报告 `ALIVE`、等待 release，最后调用 `__someboot_secondary`。因此 `ALIVE` 证明目标 CPU 已到达 final stack/page-table/common-entry 边界；它不证明 scheduler、runtime IRQ、task queue 或其他 OS per-CPU 服务已经 online。

## 架构边界

`ArchTrait::kick_secondary_cpu` 只负责 transport：

- AArch64：PSCI `CPU_ON`；
- RISC-V：SBI HSM `hart_start`；
- LoongArch：boot argument mailbox 和 boot IPI；
- x86_64：共享 trampoline 准备、INIT assert/deassert 和两次 SIPI。

x86 SIPI 使用 `0x600 | vector`。x86 不再维护 `AP_BOOTED_ID`、架构私有启动锁或 500 ms AP 轮询。通用 `CPU_BOOT_LOCK` 串行化完整 prepare/kick/wait/release 流程，也保护 x86 共享 trampoline 在前一个 AP 到达公共入口前不被覆盖。

## 平台 alive 与 scheduler online

这两个状态源表达不同事实，不能合并：

- someboot `ALIVE/SHOULD_ONLINE` 只属于启动 handoff，回答“该 CPU 是否到达平台同步点，以及 BSP 是否允许它进入 OS”。
- OS/runtime scheduler online 回答“该 CPU 的 scheduler、IRQ、timer 和运行时服务是否已经完成发布，可否接收普通工作”。

someboot 不读取 scheduler online 来完成启动握手，OS 也不把 `SHOULD_ONLINE` 当作最终 online publication。这样避免循环依赖：OS online 初始化必须先获得 someboot release 才能执行，而 someboot release 不能反过来等待 OS online。

## 失败与兼容性

`CpuOnError::AliveTimeout` 携带 logical CPU 和 hardware ID。超时上限固定为 10 秒，不执行 retry、备用 transport 或假成功。transport 自身错误继续由架构 hook 映射为 `NotSupported`、`AlreadyOn`、`InvalidParameters` 或带上下文的 `Other`。

这是 someboot 内部架构 trait 的破坏性重命名，但四个实现和唯一调用方在同一变更中迁移；没有外部稳定 ABI。`PerCpuMeta` 的布局和含义保持不变。

## 方案比较

| 方案 | 结论 |
| --- | --- |
| 保留各架构私有完成信号 | 拒绝；四架构对“启动完成”的语义继续不同，x86 全局状态仍是额外 owner。 |
| 把状态加入 `PerCpuMeta` | 拒绝；会混合 immutable trampoline ABI 与 mutable synchronization。 |
| 等待 OS scheduler online | 拒绝；会让平台 release 与 OS 初始化形成循环依赖。 |
| 超时后 retry 或 fallback | 拒绝；可能对已运行但尚未报告的 CPU 重复 kick，并掩盖真实 bring-up 失败。 |
| someboot 独立 `CpuBootSync` | 采用；状态按 CPU 隔离，transport 和 lifecycle 边界清楚，并可在最低层确定性测试。 |

## 验证

- x86 单元测试验证 SIPI base 为 `0x600` 且不含 level-assert 位；旧 `0x4600` 必然失败。
- 状态机单元测试验证只有报告 `ALIVE` 的同一 `CpuBootSync` 可以 release，并覆盖非法顺序和未发布 CPU。
- `cargo test -p someboot` 覆盖状态、layout 和现有启动契约。
- `cargo xtask clippy --package someboot` 覆盖 crate 的 feature 组合。
- 四架构 ArceOS `task-smp-online` QEMU 用例验证实际 secondary handoff。
