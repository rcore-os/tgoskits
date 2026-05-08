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

当前原子上下文主要包括：

- IRQ 已关闭。
- preempt 已禁用。

如果在这类上下文中调用可能阻塞的路径，系统会 panic，并打印 IRQ 状态和 preempt 计数。

典型覆盖路径包括：

- `ax_task::yield_now`
- `ax_task::sleep_until`
- `ax_task::exit`
- `WaitQueue::wait*`
- `TaskInner::join`
- `future::block_on`
- `ax-sync::Mutex::lock` / `try_lock`
- Starry 用户内存访问和 page fault slow path

主要入口：

- `os/arceos/modules/axtask/src/api.rs`
- `os/arceos/modules/axtask/src/wait_queue.rs`
- `os/arceos/modules/axsync/src/mutex.rs`
- `os/StarryOS/kernel/src/mm/access.rs`

后续改进方向：

- 扩展覆盖更多可能睡眠的内核 API，特别是跨模块间接阻塞路径。
- 改进 panic 信息，输出调用点、当前任务和持锁状态，降低定位成本。
- 梳理确实必须绕过检查的内部调度路径，减少 `yield_now_unchecked` 这类例外入口的使用面。

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

## 3. Task Stack Canary 检查

task stack canary 用来发现任务栈溢出或栈底被破坏。

启用 `stack-canary` 后，任务栈底会写入固定 magic 值。每次任务切换时，调度器检查上一个任务的 canary 是否仍完整；如果 magic 被覆盖，说明栈可能已经越界或被破坏，系统会 panic 并打印任务名、栈范围和期望 magic。

当前 `ax-task` 的 `multitask` feature 会启用 `stack-canary`。

覆盖范围包括：

- 动态分配的普通任务栈。
- 主 CPU 的 boot stack。
- secondary CPU 的 boot/idle stack。
- `plat-dyn` 场景下由平台提供的 secondary boot stack。

主要入口：

- `os/arceos/modules/axtask/src/task.rs`
- `os/arceos/modules/axtask/src/run_queue.rs`
- `os/arceos/modules/axruntime/src/mp.rs`
- `platform/axplat-dyn/src/boot.rs`

后续改进方向：

- 在更多边界点触发检查，例如任务退出、panic 前诊断或长时间运行的 idle 路径。
- 评估增加 guard page 或红区方案，用硬件页表保护补强 canary 的事后检测。
- 完善不同架构和不同平台栈布局的文档，明确 canary 写入位置和误报边界。

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

## 5. [`lockdep` 锁依赖检查](https://github.com/rcore-os/tgoskits/blob/dev/test-suit/arceos/rust/task/lockdep/README.md)

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
- `components/kspin/src/lockdep.rs`
- `os/arceos/modules/axsync/src/lockdep.rs`
- `os/arceos/modules/axtask/src/api.rs`
- [`test-suit/arceos/rust/task/lockdep/`](https://github.com/rcore-os/tgoskits/tree/dev/test-suit/arceos/rust/task/lockdep)

后续改进方向：

- 增加更完整的 lock class 标注能力，支持同一代码位置创建的多类动态锁。
- 扩展覆盖范围到更多同步原语，例如 rwlock、wait queue、futex 或文件系统内部锁。
- 改进 CI 策略，保留默认关闭的同时增加按需 lockdep 回归矩阵或夜间检测。
- 优化违例诊断输出，关联任务、CPU、锁类型和历史依赖路径，提升复杂 ABBA 问题的可读性。

## CI 默认启用边界

除 `lockdep` 外，这些机制已进入默认 CI 覆盖范围：`sync-lint` 作为独立 CI job 运行，panic/oops 递归保护随 runtime 默认编译；`might_sleep` 与 task stack canary 在默认 CI 的 `multitask` 构建中启用。

需要注意的是，`might_sleep` 与 task stack canary 并不是对所有单线程 ArceOS 测试包无条件启用。它们覆盖 StarryOS、Axvisor 以及多数 ArceOS QEMU 测试，但不覆盖未启用 `multitask` 的单线程测试包。

`lockdep` 由于运行时开销、诊断输出和行为侵入性更强，当前不作为默认 CI feature 启用，而是通过显式 `lockdep` feature 和专门回归用例维护。
