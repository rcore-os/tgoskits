---
sidebar_position: 6
sidebar_label: "检查机制总览"
---

# 检查机制总览

**检查**是与**测试**并列的持续保障内核质量的机制。与**测试**这种事后验证方式不同，**检查**通常是预先建立规则，以更主动的方式提前防止或发现问题。项目开发中最常见的检查机制是 `assert` 断言。

从 2026-04-01 以来，项目在检查机制上做了一批增强。主线是参照 Linux lockdep 的思路，在 ArceOS / StarryOS 中建立锁规则检查机制，用于主动发现多核并发条件下的锁违例，弥补事后测试机制的不足。

在实现 `lockdep` 的过程中，也陆续暴露出一些会影响内核并发可靠性、并阻碍 `lockdep` 自身落地的问题。因此，已经提前引入了一些静态检查、运行时检查和异常路径防护机制，并分别合并到 `dev` 分支。

以下先讨论这些相对简单的检查机制，再说明 `lockdep` 本身的实现情况。

## 1. `might_sleep` 原子上下文检查

`might_sleep` 用来检查当前代码是否在原子上下文中执行了可能睡眠或重调度的操作。

重构后的 portable `ax-task` 通过 `TaskRuntime::in_hard_irq` 拒绝 hard IRQ
中的 yield、park、sleep、exit 和 PI block，fallible API 返回
`TaskError::UnsafeContext`。StarryOS 的 `might_sleep()` 当前也显式检查 hard
IRQ。IRQ-disabled、普通 preempt-disabled task context 和“仅因持有 raw spin
lock 而不能睡眠”的统一诊断尚未重新接入，因此不能把旧调度器曾打印的完整
`preempt_count`/held-lock 字段当成当前保证。

当前实现还没有完整覆盖所有“不能睡眠”的语义来源。特别是：

- `SpinRaw` / `SpinRwLock<NoOp>` 这类 non-sleep lock 不一定改变 IRQ 或 preempt 状态；当前 lockdep build 已能在其他 atomic 条件触发时打印 held-lock stack，但还没有把 held non-sleep lock 本身作为直接触发条件。
- 用户内存 fault、可能触发 reclaim 的分配、必须原子执行的 hook 入口还缺少独立语义注解。
- preempt-disable 来源仍需后续阶段补充到诊断中。

典型覆盖路径包括：

- `ax_runtime::task::yield_current_cpu`
- `ax_task::sleep_until`（经 runtime-backed facade）
- `ax_runtime::task::exit_current`
- `WaitQueue::wait*`
- `ax_runtime::task::wait_thread` / `join_thread`
- Starry `task::future::block_on`
- `ax-sync::Mutex::lock`
- Starry 用户内存访问和 page fault slow path

`ax-sync::Mutex::try_lock` 不属于覆盖路径。它是单次 CAS，不会阻塞或睡眠，因此保持可在原子上下文中调用，语义接近 Linux `mutex_trylock`。

主要入口：

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

后续改进方向：

- 继续补 QEMU 级 IRQ handler 回归，验证显式 IRQ context 路径。
- 继续实现 held non-sleep lock 的直接判定，特别是 `SpinRaw`、`SpinRwLock<NoOp>` 和后续项目内 non-sleep rwlock。
- 继续改进 panic 信息，输出 preempt-disable 来源。
- 增加 `might_fault()`、`might_alloc()`、`cant_sleep()` / non-block scope 等语义注解，减少跨模块间接阻塞路径的盲区。
- 明确启动阶段 sleepability，区分早期启动限制和真实运行期 atomic sleep bug。
- 补充针对性回归，覆盖 IRQ handler、持 non-sleep lock、faultable user copy、阻塞式分配和 `try_lock` 非阻塞语义。

详细计划和逐项讨论状态见 [`might_sleep` 后续增强计划](./might-sleep-followups.md)。本文只保留机制级总览，避免与详细计划重复维护。

## 2. `sync-lint` 原子内存序静态检查

[`sync-lint`](/community/sync-lint) 是仓库内的静态检查工具，入口命令是：

```bash
cargo xtask sync-lint
```

它检查承担同步语义但仍使用 `Ordering::Relaxed` 的高风险模式。

当前规则包括：

- `suspicious_relaxed_wait_condition`：在等待条件或阻塞循环条件中使用 `Relaxed load`。
- `suspicious_relaxed_publish_before_notify`：`Relaxed` 写入状态后立刻 `notify` / `wake`。
- `suspicious_relaxed_mixed_ordering`：同一个同步原子变量混用强序和 `Relaxed`。

这类检查的目标不是禁止 `Relaxed`，而是筛出“这个原子变量已经在控制任务/线程/CPU 推进，但内存序仍过弱”的可疑代码。

主要入口：

- `scripts/axbuild/src/sync_lint.rs`
- [`docs/community/sync-lint.md`](/community/sync-lint)
- `.github/workflows/ci.yml`

后续改进方向：

- 扩展跨函数和跨模块的同步变量识别，减少只在单文件内判断的局限。
- 增加更多高置信模式，例如 publish 后通过 IPI、signal 或其他调度事件唤醒观察者。
- 改进忽略注释的审计能力，让长期保留的 `sync-lint: ignore` 更容易被复查。

## 3. [Task Stack Guard Page 与 Stack Protector](./task-stack-guard-page.md)

`ax-task` 现在只拥有 opaque `StackHandle`，不再内置 task-stack canary 或
`multitask` operational feature。创建线程时，portable core 通过
`StackRequest` 向 runtime 申请栈和可选 guard 区；`ax-runtime` 负责页面分配、
上下文栈顶、page-fault 诊断和最终回收。

`stack-guard-page` 在动态 runtime task/idle 栈底保留不可访问 guard 区，栈向下
越界时由 `ax-runtime::task::diagnose_current_stack_guard_page_fault()` 报告。
bootstrap 线程借用平台 boot context/stack，不由该动态分配器提供 guard page。

`stack-guard-page` 当前是 opt-in hardening feature，默认构建和普通回归测试不会启用。ArceOS Rust 应用通常通过 `ax-std/stack-guard-page` 手动启用；StarryOS 应通过 `starry-kernel/stack-guard-page` 启用，以同时打开 Starry fault handler 中的 guard page 诊断路径和底层 `ax-runtime/stack-guard-page`。项目 xtask/axbuild 流程可使用 `FEATURES=...` 注入这些 feature。

guard page 覆盖 `ax-runtime` 为普通线程和 idle 线程分配的栈，不覆盖平台
boot stack，也不覆盖未来可能引入的独立 IRQ、exception 或 overflow stack。
线程退出后只有在 wake/header 引用安全回收后，runtime 才销毁 context、TLS 和栈。

Linux 的栈保护包含两层不同机制。`STACK_END_MAGIC` 用于检查任务栈底是否
被覆盖，作用与当前 `stack canary` 接近；`CONFIG_STACKPROTECTOR` /
`CONFIG_STACKPROTECTOR_STRONG` 则依赖编译器在函数栈帧中插入 canary，
函数返回前比较保存值和运行时 guard，失败时调用 `__stack_chk_fail()`。
后者可以发现尚未触碰 guard page 的函数局部栈溢出。项目已提供 opt-in
`stack-protector` feature：构建系统注入 `-Zstack-protector=strong`，
`ax-runtime` 提供 `__stack_chk_guard` 和 `__stack_chk_fail()`。当前 nightly 对
项目使用的
`x86_64-unknown-none`、`riscv64gc-unknown-none-elf`、
`aarch64-unknown-none-softfloat`、`loongarch64-unknown-none-softfloat`
四个目标都接受该参数，形成四架构共同的全局 guard 闭环。后续再评估 Linux
风格 per-task 或 per-CPU
guard：x86_64、riscv64、aarch64 可结合各自 percpu / thread pointer /
系统寄存器约定逐步设计；loongarch64 在 Linux 6.12 中也主要体现为全局
`__stack_chk_guard` 路径，建议放在全局方案稳定后再单独评估。

平台 boot context/stack 仍必须由当前 CPU 自己使用。`ax-runtime` 将 bootstrap
和 idle scheduler record 固定到 owner CPU，防止 bring-up continuation 在另一 CPU
的 boot resources 上恢复；动态 idle 栈大小统一来自 runtime task-stack 配置。

主要入口：

- `components/ax-task/src/thread_start.rs`
- `os/arceos/modules/axruntime/src/task.rs`
- `os/arceos/modules/axruntime/src/stack_protector.rs`
- `scripts/axbuild/src/build/info.rs`

后续改进方向：

- 持续完善动态任务栈 guard page 的 SMP shootdown、跨架构 QEMU 回归和 fault 诊断。
- 后续在 `axmm` 上补 kernel vmap allocator，把 guard page 从额外物理页演进为仅占虚拟地址空间的空洞。
- 在 vmap-style 栈和 stack metadata 稳定后，再评估 boot 栈以及专用 IRQ/exception/overflow 栈的 guard page 接入。
- 在全局 stack protector 稳定后再评估 per-task/per-CPU guard。

## 4. [Panic/Oops 递归保护](./panic-recursion-guards.md)

panic/oops 递归保护用于提升异常路径健壮性，避免主故障之后在 panic 打印、backtrace 或输出锁路径中继续触发次生故障。

当前机制包括：

- panic 主路径所有权：只有一个 CPU 执行完整 panic handler。
- 递归/并发 panic 降级：同 CPU 递归 panic 或其他 CPU 并发 panic 不再进入完整打印和 backtrace 流程，而是本地 halt。
- oops 状态标记：panic/oops 期间向日志和 backtrace 路径暴露全局状态。
- panic backtrace one-shot：panic 路径最多尝试一次 backtrace。
- panic/oops 输出降级：`axlog` 在 oops 状态下绕过普通 print lock。

它的目标不是隐藏 panic，而是让系统在已经失败时尽量保持输出路径可控，减少 lockdep 违例、page fault、串口卡死等次生问题。

主要入口：

- `components/axpanic/src/lib.rs`
- `os/arceos/modules/axruntime/src/lang_items.rs`
- `os/arceos/modules/axlog/src/lib.rs`
- `components/axbacktrace/src/lib.rs`
- [`docs/docs/design/debug/panic-recursion-guards.md`](./panic-recursion-guards.md)

后续改进方向：

- 为 panic/oops 路径提供更底层的 console fast path，进一步减少对普通日志路径的依赖。
- 细化 backtrace 策略，例如按平台、构建配置或异常类型选择是否打印完整 backtrace。
- 将 BUG、die、fatal trap 等更多异常入口纳入统一的 oops 状态管理。

## 5. [Backtrace Host 符号化](./backtrace-host-symbolize.md)

Host 端 `cargo xtask backtrace symbolize` 用于对 target 输出的 raw backtrace 块（`BACKTRACE_BEGIN` / `BT` / `BACKTRACE_END`）做离线符号化，与 Issue #146、PR #635 / #646 配套。当前需 QEMU 后手动执行 symbolize；跑完测试自动 symbolize 计划在 #635 与 #646 合入后由后续 PR 提供。

主要实现：`scripts/axbuild/src/backtrace.rs`。

## 6. [`lockdep` 锁依赖检查](https://github.com/rcore-os/tgoskits/blob/dev/test-suit/arceos/rust/task/lockdep/README.md)

`lockdep` 用来检查锁使用是否违反依赖关系，是当前几类机制中最完整的运行时锁检查框架。

它检查的问题包括：

- 递归加锁。
- ABBA 锁顺序反转。
- 乱序解锁。
- held-lock 栈溢出。
- spin lock 与 mutex 混合使用时的锁顺序反转。

当前实现已经抽出独立 `ax-lockdep` 组件，使用 task-held tracking 记录当前任务持有的锁，并通过 lock class / lock instance 区分锁顺序关系和具体锁实例。

检查流程大致是：

1. 加锁前生成 held-lock snapshot。
2. 根据当前请求锁的 class 和已持有锁栈检查递归加锁或顺序反转。
3. 加锁成功后记录依赖边，并把锁压入当前任务 held-lock 栈。
4. 解锁时检查释放顺序是否与栈顶一致。
5. 发现违例时打印 requested lock、conflicting held lock 和 held stack。

接入范围包括：

- `ax-kspin` spin lock。
- `ax-sync` mutex。
- POSIX pthread mutex lockdep-aware 布局。
- ArceOS lockdep QEMU 回归用例。

主要入口：

- `components/lockdep/src/state.rs`
- `components/lockdep/src/trace.rs`
- `components/kspin/src/context.rs`
- `components/kspin/src/runtime_call.rs`
- `components/kspin/src/wrapper.rs`
- `os/arceos/modules/axsync/src/lockdep.rs`
- `os/arceos/modules/axruntime/src/guard.rs`
- [`test-suit/arceos/rust/src/lockdep/`](https://github.com/rcore-os/tgoskits/tree/dev/test-suit/arceos/rust/src/lockdep)

后续改进方向：

- 增加更完整的 lock class 标注能力，支持同一代码位置创建的多类动态锁。
- 扩展覆盖范围到更多同步原语，例如 rwlock、wait queue、futex 或文件系统内部锁。
- 改进 CI 策略，保留默认关闭的同时增加按需 lockdep 回归矩阵或夜间检测。
- 优化违例诊断输出，关联任务、CPU、锁类型和历史依赖路径，提升复杂 ABBA 问题的可读性。

外部 `spin` 迁移后留下的锁类型、锁范围和原子上下文 follow-up 统一记录在
[`锁使用问题跟踪`](./lock-usage-followups.md)。

## CI 默认启用边界

除 `lockdep` 外，这些机制已进入默认 CI 覆盖范围：`sync-lint` 作为独立 CI job 运行，panic/oops 递归保护随 runtime 默认编译；`multitask` 构建还会覆盖 `ax-task` 的 hard-IRQ 上下文检查、runtime 栈 guard page，以及编译器 stack protector。

需要注意的是，这些任务上下文与任务栈检查并不对所有单线程 ArceOS 测试包无条件启用。它们覆盖 StarryOS、Axvisor 以及多数 ArceOS QEMU 多任务测试，但不覆盖未启用 `multitask` 的单线程测试包。

`lockdep` 由于运行时开销、诊断输出和行为侵入性更强，当前不作为默认 CI feature 启用，而是通过显式 `lockdep` feature 和专门回归用例维护。
