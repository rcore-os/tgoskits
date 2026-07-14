---
sidebar_position: 8
sidebar_label: "might_sleep 后续计划"
---

# `might_sleep` 后续增强计划

本文档记录 `might_sleep` 原子上下文检查的后续增强计划，用作逐项讨论和拆分实现任务的基础。

调度重构后，portable `ax-task` 不再读取 HAL 或 OS 的全局
`preempt_count`。它通过 `TaskRuntime::in_hard_irq` 拒绝 hard IRQ 中的
yield、park、sleep、exit 等入口，并以 `TaskError::UnsafeContext` 暴露可恢复错误；
`WaitQueue` 的 infallible 包装再把不变量错误转成 panic。StarryOS 的
`task::might_sleep()` 当前同样只显式检查 hard IRQ。IRQ-disabled、持有
non-sleep lock 和普通 preempt-disabled task context 的统一诊断仍属于本文的
后续工作，不能沿用旧调度器的实现状态描述。

这个基础机制已经能发现一批典型错误，但还没有覆盖所有“不能睡眠”的语义来源。后续增强重点不是机械增加更多 `might_sleep()` 调用点，而是让上下文判断、诊断信息和注解能力更完整。

## 当前边界

当前机制的主要入口：

- `components/ax-task/src/facade.rs`
- `components/ax-task/src/wait_queue.rs`
- `os/arceos/modules/axruntime/src/guard.rs`
- `os/arceos/modules/axruntime/src/task.rs`
- `os/StarryOS/kernel/src/task/future.rs`
- `os/StarryOS/kernel/src/task/scheduler_task.rs`
- `os/arceos/modules/axsync/src/mutex.rs`
- `os/arceos/modules/axhal/src/irq.rs`
- `platforms/ax-plat/src/irq.rs`
- `os/StarryOS/kernel/src/mm/access.rs`

当前需要保留的语义：

- `ax_sync::PiMutex::lock` 在竞争时通过 `ax-task` PI block/handoff API 睡眠，不再调用一个
  OS-global `might_sleep()`；hard IRQ 会由调度 API 拒绝。
- `ax_sync::PiMutex::try_lock` 只检查本地 owner，是非阻塞尝试，不应引入
  sleepability 检查，
  语义接近 Linux `mutex_trylock`。
- `ax_sync::SpinMutex` / 兼容 `Mutex`、`SpinNoIrq` / `SpinNoPreempt` /
  `SpinRaw` / `SpinRwLock` 自身不睡眠；
  持锁后进入睡眠路径的统一诊断尚未完成，必须由 runtime guard/lockdep 状态补齐。
- Starry `future::sleep()` 是 async future，本身不直接阻塞；
  `future::block_on()` 只在所属线程 poll，并通过 runtime scheduler facade park/wake。

## 计划清单

| 编号 | 优先级 | 项目 | 当前问题 | 讨论状态 |
| --- | --- | --- | --- | --- |
| MS-1 | P0 | 纳入显式 IRQ context | 已通过 `ax_hal::irq::in_irq_context()` 纳入 `might_sleep()` / `in_atomic_context()` 判定。 | 已完成 |
| MS-2 | P0 | 识别 held non-sleep lock | Phase 1 已在 lockdep build 下输出 held-lock stack；直接把 held non-sleep lock 作为触发条件仍待 Phase 2。 | Phase 1 已完成 |
| MS-3 | P0 | 改进 panic 诊断 | Phase A/B 已输出 caller、reason、IRQ context、preempt count、CPU、task id、task state、held locks；preempt-disable 来源待后续阶段。 | Phase A/B 已完成 |
| MS-4 | P1 | 增加 `might_fault()` 注解 | 用户内存访问和 page fault slow path 已手动调用 `might_sleep()`，但缺少表达“这里可能 fault”的独立注解。 | 方向已确认 |
| MS-5 | P1 | 增加 `might_alloc()` 注解 | 页分配失败后可能进入 page cache reclaim，reclaim 可能调用复杂路径；当前没有统一标注“阻塞式分配/回收风险”。 | 方向已确认 |
| MS-6 | P1 | 增加 `cant_sleep()` / 原子断言 | IRQ、kprobe、perf、stop_machine 等必须原子执行的入口缺少统一“这里必须不能睡眠”的反向断言。 | 方向已确认 |
| MS-7 | P2 | 明确启动阶段 sleepability | rootfs mount、pseudofs init、tmpfs root_dir 等路径需要区分早期启动限制和真实运行期 atomic sleep bug。 | 方向已确认 |
| MS-8 | P2 | 补充针对性回归 | 现有回归主要验证功能路径，缺少专门触发 `might_sleep()` 违例的最小用例矩阵。 | 方向已确认 |
| MS-9 | P3 | 文档和审计命令更新 | 现有文档需同步当前源码语义，并沉淀复查命令。 | 方向已确认 |

## MS-1：纳入显式 IRQ context

平台 IRQ 框架已经维护显式 IRQ 上下文：

- `platforms/ax-plat/src/irq.rs` 中 `IN_IRQ_CONTEXT` 记录当前 CPU 是否正在 IRQ dispatch。
- `dispatch_irq()` 进入时置位，返回前恢复。
- `IrqOps::in_irq_context()` 已被 IRQ 注册、释放、同步等路径使用。

`ax_hal::irq::in_irq_context()` 已存在；当前 portable core 通过
`TaskRuntime::in_hard_irq` 接收等价能力，Starry wrapper 则通过
`ax_runtime::hal::irq::in_irq_context()` 检查。

实现状态：

- 已在 `platforms/ax-plat/src/irq.rs` 暴露 `in_irq_context()`。
- 已在 `os/arceos/modules/axhal/src/irq.rs` re-export。
- 已在 `components/ax-task` 的 facade/wait queue 中返回
  `TaskError::UnsafeContext`，并在 Starry `scheduler_task::might_sleep()` 中纳入显式 IRQ context。

已确认方向：

- 在 `platforms/ax-plat/src/irq.rs` 暴露 `in_irq_context()`，返回当前 CPU 的 `IN_IRQ_CONTEXT` 状态。
- `os/arceos/modules/axhal/src/irq.rs` re-export 该接口；`ax-runtime`
  将它实现为 `TaskRuntime`/`LockRuntime` 能力，`ax-task` 不依赖 `ax-hal`。
- OS facade 负责决定哪些错误转为诊断或 panic，portable core 保持 typed error。
- `might_sleep()` panic 信息同步打印显式 IRQ context。
- 不用 `NoPreempt` 语义替代 IRQ context；`NoPreempt` 只是当前 IRQ handler 外层实现细节。

讨论点：

- 是否需要为非 `irq` feature 提供恒为 `false` 的 stub，还是只在调用侧 `cfg(feature = "irq")`。
- VM exit 转发 IRQ、IPI handler、timer IRQ 的覆盖范围验证仍需后续 QEMU 回归覆盖。

完成标准：

- `might_sleep()` 在显式 IRQ context 中必然触发，即使局部 IRQ 状态未来发生变化。
- 新增最小回归覆盖 IRQ handler 内调用睡眠入口的错误路径。

## MS-2：识别 held non-sleep lock

当前 `lockdep` feature 下，task 已经有 held-lock stack，`ax-kspin` 和 `ax-sync` 会在加锁/解锁时维护 held lock。这个信息可以用于增强 `might_sleep()` 诊断和判定。

实现状态：

- `ax-lockdep::HeldLock` 已记录 kind、`sleep_forbidden`、class、addr、acquired_at。
- `ax-kspin` 的 spin / spin-rwlock 记录为 `sleep_forbidden=true`。
- `ax_sync::SpinMutex` 和兼容 `Mutex` 经 `ax-kspin` 记录为
  `sleep_forbidden=true`；只有显式 `ax_sync::PiMutex` 记录为
  `sleep_forbidden=false`，避免把 sleepable mutex 本身误标成 non-sleep lock。
- `might_sleep()` 在 `lockdep` feature 下 panic 时会打印当前 held-lock snapshot。
- 当前完成的是诊断增强；`SpinRaw` / `SpinRwLock<NoOp>` 持锁睡眠的直接判定仍留给第二阶段 `non_sleep_lock_depth` 或等价状态。

需要重点覆盖的锁：

- `SpinNoPreempt`
- `SpinNoIrq`
- `SpinRaw`
- `SpinRwLock`
- `SpinNoIrqRwLock`
- 后续可能新增的项目内 non-sleep rwlock

建议方向：

- 在 held lock 记录中增加锁 kind 或 sleepability 标记。
- 或在 `ax-runtime` 的 per-CPU guard state 中维护轻量
  `non_sleep_lock_depth`，由 `LockRuntime`/kspin acquire/release 更新。
- `try_lock` 失败不应留下 held 状态；try 成功后与普通 lock 一样记录。

已确认方向：

- 第一阶段已完成：增强 `lockdep` 构建下的诊断，复用现有 task held-lock stack，在 `might_sleep()` 失败时打印当前 held locks。
- 第一阶段已完成：给 held lock 补充最少语义字段，能区分 `spin` / `mutex` / `spin-rwlock` 以及该锁是否 `sleep_forbidden`。
- 第二阶段再增加可选的轻量 `non_sleep_lock_depth` 或等价状态，由
  `ax-kspin` 通过 `LockRuntime` 通知 OS runtime；`ax-task` 不能反向依赖锁 crate。
- 第二阶段应通过 feature 控制，避免无条件增加所有 spin lock 快路径成本。
- 不把 `SpinRwLock` read guard 直接机械塞进 lockdep dependency stack 来解决睡眠检查。读写锁依赖检查和“持锁禁止睡眠”是相关但不同的语义，应共享诊断信息而不是强行共用同一个判定模型。

讨论点：

- 第一阶段 held-lock 输出格式如何和 `ax-lockdep` 现有格式复用。
- 第二阶段 feature 名称和默认启用范围。
- raw lock、读锁和 IRQ-only 短锁的误报边界如何控制。

完成标准：

- 持有 non-sleep lock 后调用 `might_sleep()` 能报告问题。
- 报告能指出至少一个持有锁的 acquire 位置。
- 不改变正常锁快路径的默认开销，或开销可通过 feature 控制。
- 已新增 host 单测覆盖持 `SpinNoPreempt` 时 `might_sleep()` 输出 held-lock stack。

## MS-3：改进 panic 诊断

当前 panic 信息定位成本仍偏高。Linux `__might_resched()` 会输出调用点、`in_atomic`、IRQ 状态、non-block 计数、preempt count、held locks、preempt-disable 位置和栈。

本项目建议分阶段补齐：

1. 第一阶段：输出 `#[track_caller]` caller、task name、task state、IRQ context、IRQ enabled、preempt count、CPU id、task id。
2. 第二阶段：在 `lockdep` feature 下输出 held-lock stack。
3. 第三阶段：记录第一次 `disable_preempt()` 的 caller，输出 preempt-disabled 来源。

已确认方向：

- 阶段 A 已完成：输出不依赖额外锁路径的上下文快照，包括 caller、IRQ enabled、显式 IRQ context、preempt count、CPU id、task id、task state。
- 阶段 A 已完成：输出结构化 reason 列表，目前覆盖 `irq_disabled`、`irq_context`、`preempt_disabled`，避免只打印“atomic context”这个总称。
- 阶段 B 已完成：在 `lockdep` feature 下打印 held-lock stack，复用 `ax-lockdep` 字段，包含 kind、sleepability、class、addr、acquired_at。
- 阶段 C 在 preempt count 从 0 变成 1 时记录 preempt-disable caller，在降回 0 时清除；`might_sleep()` 因 preempt disabled 触发时输出该位置。
- 阶段 C 如果 `#[track_caller]` 不能完整穿透 `PreemptGuard::new()` / `PreemptIrqGuard::new()` 到 runtime 的 preempt hook，先记录 guard 创建点，不伪造更精确的位置。
- 阶段 A 可以与 MS-1 同一 PR 完成；阶段 B 跟随 MS-2 第一阶段；阶段 C 单独拆分。

讨论点：

- 是否使用普通 `panic!` 输出，还是在 oops 状态下走更底层 console fast path。
- held-lock stack 输出格式复用 runtime 的固定 lockdep ring，还是在 OS 诊断层定义轻量格式。
- panic 路径是否需要 rate limit 或 one-shot。

完成标准：

- 单看 `might_sleep()` panic 日志能判断是哪类原子上下文触发。
- 若由持锁导致，能看到相关锁 acquire 位置。
- 已新增 host 单测覆盖 preempt-disabled 场景下的 reason、caller、preempt count 和 task state。
- 已新增 host 单测覆盖 lockdep held-lock stack 输出。

## MS-4：增加 `might_fault()` 注解

Starry 用户内存访问和 page fault slow path 目前直接调用 `might_sleep()`。建议增加语义更明确的 `might_fault()`，内部先复用 `might_sleep()`。

`might_fault()` 的含义：

- 它标注“当前位置接下来可能访问 faultable memory，并可能因为处理 page fault 进入会阻塞或重调度的 slow path”。
- 它不是普通调度 API，而是用户内存访问、copy helper、page fault slow path 这类路径的语义注解。
- 第一版实现仍然只复用 `might_sleep()` 的原子上下文检查，不引入完整 Linux `pagefault_disable()` / `faulthandler_disabled()` 模型。

`might_fault()` 的作用：

- 让代码审计时能直接区分“这里可能主动睡眠”和“这里可能因 fault 间接睡眠”。
- 把 faultable user memory access 的检查入口统一起来，后续如果要增加 pagefault-disabled 状态、地址空间锁诊断或用户内存访问策略，可以从这个注解点扩展。
- 保持现有 `might_sleep()` 诊断能力，避免每个用户访问路径重复写低层 atomic-context 检查。

已确认方向：

- 在 `ax_task` 中提供 `#[track_caller] pub fn might_fault()`，第一版内部只调用 `might_sleep()`。
- Starry 用户内存访问入口改用 `might_fault()`，包括 `access_user_memory()` 和 page fault slow path。
- `might_fault()` 不替代 `access_user_memory()` 对 IRQ enabled、thread context、地址空间锁等 Starry 边界条件的检查。
- 第一版不实现 `pagefault_disabled()` 计数，也不把 Starry 特定的 aspace/thread 判断塞回 `ax_task`。

适合接入的路径：

- `os/StarryOS/kernel/src/mm/access.rs`
- 用户态 copy helper。
- page fault slow path。

讨论点：

- 后续是否需要引入 pagefault-disabled 状态，以及它应放在 Starry 还是通用 task 层。
- 地址空间锁递归、当前线程不允许 user fault 等 Starry 特定条件应继续返回错误、warn 还是 panic。

完成标准：

- faultable user memory access 的检查入口语义统一。
- 文档和注释不再把所有 faultable 场景都泛化成普通 sleep。

## MS-5：增加 `might_alloc()` 注解

页分配失败后可能触发 page cache reclaim，例如 `axalloc` 的 `alloc_pages()` 会在失败后调用 `try_page_reclaim()`。这类路径不一定立即睡眠，但可能进入文件系统、page cache、回调和释放路径。

`might_alloc()` 的含义：

- 它标注“当前位置可能执行阻塞式或 reclaim 型分配，调用点必须处于允许睡眠/重调度的上下文”。
- 第一版目标是覆盖可能进入 reclaim、文件系统、回调或等待路径的分配，不是捕捉所有普通堆分配。
- 它接近 Linux `might_alloc(GFP_KERNEL)` 的用途，但当前项目还没有统一 GFP/flags 模型，因此第一版不引入复杂参数。

建议先只覆盖“可能进入复杂 reclaim 的页分配”：

- `os/arceos/modules/axalloc/src/buddy_slab.rs`
- `os/arceos/modules/axalloc/src/tlsf_impl.rs`
- `os/arceos/modules/axfs-ng/src/file/page.rs`

已确认方向：

- 在 `ax_task` 中提供 `#[track_caller] pub fn might_alloc()`，第一版内部只调用 `might_sleep()`。
- 先接入页分配失败后可能触发 reclaim 的路径，以及明确用于 page cache / fs page 的分配入口。
- 不全局 hook Rust allocator 的每次 `alloc()`。
- 不在普通 `Vec` / `BTreeMap` / `Arc` 分配点到处手工添加。
- 不把 `try_reserve` 这类局部 OOM 处理点都标成 blocking allocation，除非该路径会进入 reclaim 或 wait path。
- 后续如果引入 `AllocFlags::{Atomic, Blocking}` 或等价模型，再考虑把 `might_alloc()` 扩展为带参数版本。

讨论点：

- 页分配失败后触发 reclaim 的入口应放在第一次分配前、失败后重试前，还是只放在进入 reclaim 前。
- 分配器在早期启动和 IRQ 路径里的合法非阻塞用法如何标注。
- 后续分配 flag 模型是否需要和 axalloc API 一起设计。

完成标准：

- 原子上下文中触发可能 reclaim 的分配能被提前报告。
- IRQ-safe、预分配或明确非阻塞的分配路径不被误伤。
- 文档明确 `might_alloc()` 第一版不是通用内存分配检查。

## MS-6：增加 `cant_sleep()` / 原子断言

`might_sleep()` 是“这里可能睡眠”的正向注解；还需要“这里必须不能睡眠”的反向断言，用于高风险入口。

需要区分两个概念：

- `cant_sleep()` 是断言。它表示“当前位置应该已经处在不能睡眠的上下文”，如果当前仍是普通 sleepable task context，则说明调用边界错误。
- `non_block_start()` / `non_block_end()` 是状态。它表示“从现在开始临时禁止阻塞”，后续 `might_sleep()` 需要检查 task 上的 non-block 计数。

已确认方向：

- 第一版只实现 `cant_sleep()`，暂不实现 `non_block_start()` / `non_block_end()`。
- `cant_sleep()` 复用当前 atomic-context 判断；在 MS-1 后，该判断包含显式 IRQ context。
- `cant_sleep()` 第一版检查当前是否满足以下任一条件：显式 IRQ context、IRQ disabled、preempt disabled，后续可加入 `non_sleep_lock_depth > 0`。
- 如果都不满足，`cant_sleep()` 报告调用点并 panic 或进入项目统一 fatal 路径。
- `non_block_start()` / `non_block_end()` 延后设计，因为它需要 task 字段、嵌套计数和异常退出恢复策略。

候选入口：

- IRQ handler。
- kprobe / retprobe handler。
- perf sampling hook。
- stop_machine callback。
- scheduler switch hook。

讨论点：

- `cant_sleep()` 失败时使用 panic、warn，还是现有 oops/fatal 路径。
- 第一批接入入口选择：通用 IRQ handler、kprobe、perf、stop_machine、scheduler switch hook 中哪些先做。
- `non_block_start()` / `non_block_end()` 后续是否需要和 task 状态、panic guard 绑定。

完成标准：

- 必须原子执行的入口可以显式表达约束。
- 后续审计时不再只依赖注释说明“这里不能睡眠”。
- 文档明确 `cant_sleep()` 和未来 `non_block_start()` / `non_block_end()` 的区别。

## MS-7：明确启动阶段 sleepability

文档中已经记录 rootfs mount、pseudofs init、tmpfs root_dir 等启动路径的 sleepability 边界还不清晰。长期方向是减少因为启动阶段限制而长期保留自旋锁。

已确认方向：

- 不做全局“boot 阶段允许 sleep”的豁免，避免隐藏真实 atomic sleep bug。
- 第一版优先增强诊断，让 `might_sleep()` 报错能区分 `no_current_task`、`idle_task`、`scheduler_not_ready`、`runtime_init` 和普通运行期 atomic context。
- 职责边界上，`ax-runtime` 维护生命周期、IRQ/preempt nesting 和 OS
  诊断；`ax-task` 只依据显式 runtime capability 与线程状态返回 typed error。
- 如果当前没有稳定的 runtime phase 信号，第一步不强行引入完整 `SleepabilityPhase`；先基于已有 current task / scheduler 状态补诊断。
- 只有遇到真实 false positive，再引入类似 `SleepabilityPhase::{NoScheduler, RuntimeInit, TaskContext, PanicOops}` 的显式阶段。
- rootfs / pseudofs / tmpfs root_dir 等启动路径不靠 `might_sleep()` 特判长期绕过；后续要么迁到普通 task context，要么明确标注它们必须走 non-sleep 路径。

讨论点：

- 第一版能否只依赖 current task、idle task、scheduler 状态完成足够诊断。
- 是否应在 `ax-runtime` facade 中增加显式生命周期快照，而不把 OS 阶段塞进 portable core。
- `might_sleep()` 在早期阶段应 panic、warn，还是带明确原因地拒绝；默认倾向保持严格失败。
- rootfs / pseudofs 初始化是否能移动到普通任务上下文。

完成标准：

- `might_sleep()` 报错能区分启动阶段限制和运行期 atomic sleep bug。
- 锁类型选择不再因为“早期启动可能误伤”而长期保守化。
- 文档明确启动阶段不是 `might_sleep()` 的默认豁免条件。

## MS-8：补充针对性回归

建议为增强项补最小回归，而不是只依赖真实系统路径偶发触发。

已确认方向：

- 新增 ArceOS rust debug 类测试 feature，不把预期 panic 用例塞进普通通过型 suite。
- 对“应该触发 `might_sleep()` panic”的用例，沿用 `lockdep-detect` 这类 xtask feature override：`success_regex` 匹配明确诊断文本，`fail_regex` 只匹配“未触发预期诊断”的兜底错误。
- 预期 panic 会终止系统，因此每个预期 panic 场景单独一个 feature/case，不放在同一个 boot 里。
- `ax_sync::PiMutex::try_lock()` 在原子上下文不触发属于通过型反例，可以放进普通
  smoke case；测试不能在成功后持 guard 进入睡眠路径。
- Starry user copy / `might_fault()` 回归放到 MS-4 实现之后补，不抢在核心 `might_sleep()` 判定测试之前。

第一批矩阵：

- IRQ handler 内调用 `ax_task::sleep()` 或 `WaitQueue::wait()` 应触发。
- preempt disabled 后调用睡眠入口应触发，覆盖现有基础路径。
- 持 `SpinNoIrq` 后调用 `ax_sync::PiMutex::lock()` 应触发，覆盖 IRQ/preempt 路径。
- `lockdep` feature 下持 `SpinRaw` / `SpinRwLock` 后调用睡眠入口应触发，覆盖 MS-2 的 held non-sleep lock 判定。
- `ax_sync::PiMutex::try_lock()` 在原子上下文中不应触发，防止把 non-blocking fast
  path 误判为 sleepable 操作。

讨论点：

- 测试目录放在 `test-suit/arceos/rust/src/debug/` 下，还是新增 `src/might_sleep/`。
- 每个预期 panic feature 的命名，例如 `debug-might-sleep-irq`、`debug-might-sleep-preempt`、`debug-might-sleep-lockdep-held-lock`。
- 后续 MS-3 诊断格式稳定后，success regex 是否改成匹配结构化 reason 字段。

完成标准：

- 每类新增判定至少有一个确定性回归。
- 默认 CI 覆盖一部分，lockdep feature 下覆盖 held-lock 诊断。
- `PiMutex::try_lock()` 反例持续证明 non-blocking fast path 不被误伤。

## MS-9：文档和审计命令更新

需要同步维护以下文档：

- `docs/docs/debug/check-mechanisms-summary.md`
- `docs/docs/debug/lock-usage-followups.md`
- 本文档

已确认方向：

- 本文档作为 `might_sleep()` 后续增强的详细计划文档，记录 MS-1 到 MS-9 的讨论状态、确认方向和完成标准。
- `check-mechanisms-summary.md` 只保留总览级信息和指向本文档的链接，不复制完整计划，避免两份文档长期漂移。
- `lock-usage-followups.md` 只同步锁策略相关交叉点：held non-sleep lock、启动阶段 sleepability、spin guard 内不能 fault / alloc / I/O / callback。
- 后续每完成一个实现 PR，同步更新三类信息：源码行为、回归覆盖、计划项状态。

建议补充复查命令：

```bash
rg -n "might_sleep|might_fault|might_alloc|cant_sleep|non_block" \
  --glob '*.rs' --glob '!target/**'

rg -n "SpinNoIrq|SpinNoPreempt|SpinRaw|SpinRwLock|SpinNoIrqRwLock" \
  os components drivers net memory virtualization --glob '*.rs'

rg -n "access_user_memory|handle_page_fault|vm_read|vm_write|IoDst::write" \
  os/StarryOS/kernel/src --glob '*.rs'
```

完成标准：

- 文档描述与源码行为一致。
- 后续每完成一个计划项，同步更新本文档的讨论状态和完成状态。
- 总览文档只维护机制级摘要，本文档维护逐项计划，锁使用文档维护锁策略交叉约束。

## 暂不建议直接照搬的 Linux 机制

以下 Linux 机制值得参考，但不建议当前阶段完整照搬：

- 完整 `preempt_count` bit layout，包括 hardirq、softirq、NMI、RCU depth。当前项目还没有同等复杂的上下文模型。
- `might_resched()` / `cond_resched()` 的动态 preempt 体系。当前调度器语义不同，应先保持 `might_sleep()` 的检查职责清晰。
- warn-and-taint 模型。当前项目更倾向开发期直接 panic，后续可按场景再引入 warn 模式。

## 验证记录

2026-07-02 在实现 MS-1、MS-2 Phase 1 和 MS-3 Phase A/B 时，以下 host 单元测试过滤项出现 SIGSEGV：

- 已退役 ArceOS 调度器的 `test_fp_state_switch` host 过滤项（当时通过 `test sched-rr` feature 运行）
- `cargo test -p ax-lockdep dynamic_lock_instances_do_not_consume_class_slots`
- `cargo test -p ax-lockdep subclass_tracks_same_base_class_nesting`

已在临时 clean worktree 上用改动前 HEAD `9c8bb98d0` 复跑相同过滤项，三者同样 SIGSEGV。因此该现象不是本次 `might_sleep` 增强引入，先记录为既有 host-test 不稳定或未定义行为问题。新增/修改的过滤测试已单独通过。

## 实现拆分建议

当前 MS-1 到 MS-9 的方向均已确认。建议后续按以下顺序拆实现 PR：

1. MS-1 + MS-3 Phase A：已完成显式 IRQ context 和基础 panic 诊断字段。
2. MS-2 Phase 1 + MS-3 Phase B：已完成 lockdep build 下的 held-lock 诊断。
3. MS-8 第一批：继续补 QEMU 级 IRQ handler、held-lock 直接判定和 `try_lock` 反例回归。
4. MS-4：`might_fault()` 注解和 Starry user memory / page fault slow path 接入。
5. MS-5：`might_alloc()` 注解和 reclaim-capable 分配路径接入。
6. MS-6：`cant_sleep()` 反向断言和第一批必须原子入口接入。
7. MS-7：启动阶段 sleepability 诊断增强；只有出现真实 false positive 后再引入显式 runtime phase。
8. MS-9：每个实现 PR 同步更新本文档、总览和相关锁使用文档。
