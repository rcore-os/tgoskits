# ax-task Linux v7.1 PREEMPT_RT 系统审计

## 文档状态与范围

本文是 `codex/refactor-ax-task-from-1596` 分支的任务系统审计台账。它记录设计不变量、Linux 对照、已确认缺陷、确定性红绿测试和里程碑验证，不把历史 QEMU 结果冒充为当前提交的通过结果。

分支每次推送前都要 rebase 当时最新的 `origin/dev`。Starry 的 PID/zombie 生命周期以 dev 的 `ProcessIdentity` 为唯一权威，状态为：

```text
Live -> Zombie -> Reaping -> Reaped
```

本轮允许调整 `ax-task`、`ax-runtime`、`cpu-local` 和 Starry 进程/调度内部接口。驱动只审计分支已经触及的 serial、vsock、USB/IRQ 边界；外围缺陷单独登记 issue，不借任务调度重构扩大范围。

内部架构可以破坏性重构，但必须保持以下外部行为：

- Starry Linux ABI 与 errno；
- POSIX 和 axstd 可观察行为；
- `ax_std::os::arceos::modules::ax_task` 兼容导出；
- generation-bearing 调度身份不得直接当作 Linux TID/PID。

## Linux 参考基线

主要参考为本地 `~/linux-src`：Linux `v7.1`，提交 `8cd9520d35a6c38db6567e97dd93b1f11f185dc6`。参考配置在源码树外生成：

```text
CONFIG_NO_HZ=y
CONFIG_HIGH_RES_TIMERS=y
CONFIG_PREEMPT_RT=y
CONFIG_PREEMPT_DYNAMIC=y
CONFIG_EXPERT=y
CONFIG_SMP=y
CONFIG_HOTPLUG_CPU=y
```

重点源码：

- 调度：`kernel/sched/core.c`、`fair.c`、`rt.c`、`deadline.c`；
- PI 与锁：`kernel/locking/rtmutex.c`、`rwsem.c`、`spinlock_rt.c`；
- timer：`kernel/time/hrtimer.c`、`clockevents.c`、`tick-oneshot.c`；
- 远程投递：`kernel/irq_work.c`、`kernel/softirq.c` 和各架构 IPI；
- perf：`kernel/events/core.c`；
- 生命周期：`kernel/exit.c`、`kernel/fork.c`、`fs/exec.c`；
- IRQ/驱动：`drivers/tty/serial`、`net/core/dev.c` 及控制器 IRQ 实现。

Linux 用于核对状态所有权、锁序和发布顺序，不复制其对象布局，也不把 callback API 原样带入 crate 边界。

## 四架构统一模型

### 统一的是语义，不是物理存储

四架构应共享同一组前端语义：

1. 普通 `preempt_count` 描述当前执行上下文的嵌套深度；
2. `need_resched` 是在下一安全点进入调度器的单调请求；
3. hard-IRQ/IRQ mask、scheduler baton、runqueue owner 属于 CPU；
4. 普通 guard 的最后一层只能原子转换为 scheduler baton，不能先暴露可抢占窗口；
5. 上下文切换尾在清除前一线程物理 `on_cpu` 后，才能迁移或回收资源；
6. current-task 发布分两阶段：先发布 CPU anchor，再由架构裸切换尾更新任务寄存器。两阶段之间禁止执行可失败或会释放所有权的 Rust 代码。

物理存储由架构后端决定：

| 架构 | Linux current 来源 | Linux 普通抢占状态 | TGOSKits 后端 |
| --- | --- | --- | --- |
| x86_64 | per-CPU `current_task`，通过 GS 访问 | per-CPU `__preempt_count`，倒置的 resched 位折叠在同一字 | `CpuRuntimeAnchor` 的 GS 固定偏移 |
| AArch64 | `SP_EL0` 指向 current task | `current_thread_info()->preempt`，count 与 need-resched 均跟随任务 | `CurrentThreadHeader`，裸切换尾写 `SP_EL0` |
| RISC-V | `tp` 指向 current/task thread-info | 通用 task-owned `preempt_count`，resched 使用任务 TIF | `CurrentThreadHeader`，裸切换尾写 `tp` |
| LoongArch64 | `$tp` 指向 current thread-info | 通用 task-owned `preempt_count`，resched 使用任务 TIF | `CurrentThreadHeader`，裸切换尾写 `$tp` |

因此不采用“四架构强制同一内存位置”的模型。统一接口为 `scheduler_*preempt*`，后端选择 live word。x86 保留 GS 快路径；其余 load/store 架构让 guard 状态随任务迁移。scheduler baton 和 IRQ owner 在四架构都保持 per-CPU。

### current-thread 安全边界

`scheduler_current_thread()` 用来在尚未构造 guard 时读取 current，因此不能先依赖普通 `CpuPin`。它必须：

- 拒绝零地址和错误对齐；
- TLS/CPU-anchor 模式下重读 CPU base，迁移则重试；
- TLS 模式校验 header 仍绑定到采样的 CPU area；
- 只返回非逃逸借用，调用方不得跨上下文切换保存裸指针。

`PreparedThreadSwitch::commit()` 只发布 runtime anchor。AArch64/RISC-V/LoongArch 的裸切换尾必须随后写 current-task 寄存器；x86 的 anchor 即 GS current 来源，不需要第二个寄存器写入。漏掉第二阶段会由 `CurrentThreadMismatch` 检出。

## 总体所有权分层

```text
ax-task
  调度策略、线程状态、placement、PI、task deadline、owner-CPU inbox
        |
        | TaskRuntime 能力接口：无 OS 对象反向依赖
        v
ax-runtime
  CPU-local/bootstrap、guard/baton、物理 clockevent、IPI、stack/TLS/context
        |
        v
Starry / ArceOS / AxVM
  Linux 身份、地址空间、信号、perf、通用 timer、设备 worker
```

关键规则：

- `ax-task` 只拥有调度语义 timer，不提供任意 callback timer；
- `ax-runtime::LocalClockEvent` 是物理 oneshot 的唯一状态源；
- 任务态注册表、进程关系、perf control 等使用可睡眠 PI 锁；
- hard IRQ、scheduler、panic 路径只能使用有界原子协议、固定容量队列、`IrqWaitCell` 或极窄的 raw gate；
- wake、callback、资源释放必须发生在广域锁之外。

## 硬中断 endpoint 与独立生命周期

只要 IRQ 数据与上层对象不要求“同一临界区内强事务一致”，就必须拆成独立 endpoint，避免让硬中断进入外部锁图。

### 三层模型

1. **IRQ 私有层**：寄存器状态、固定容量队列、pending/epoch、generation-bearing 注册槽。只允许有界 claim、ACK/mask、FIFO drain 和值发布。
2. **交接层**：通过 ring、sticky bit、`IrqWaitCell` 或 IPI 发布值事件。顺序固定为“先 payload，后 signal/doorbell”。不保存上层裸指针，不运行任意 callback。
3. **任务态层**：worker 独占完整对象，或用 PiMutex 管理任务态共享状态；消费事件后再做协议解析、TTY、进程/PMU 状态、用户唤醒和可能获取外部锁的操作。

### 生命周期

推荐状态为：

```text
Unpublished -> Published -> Draining -> Dead
```

销毁顺序：

1. 关闭新发布并推进 generation；
2. mask/ACK 物理源；
3. 撤销 endpoint 可见性；
4. 等待 owner-CPU 或 hard-IRQ reader grace period；
5. 释放上层对象和存储。

`Draining` 期间允许完成已取得 lease 的读者，但拒绝新 producer。硬件要求的 raw register gate 只能覆盖有限 MMIO 序列，不能跨外部调用、wake 或资源释放。

### 当前审计结论

- scheduler IPI 已使用 payload-before-sticky、消费旧门铃后允许新投递的模型；
- deferred task-work 使用 intrusive 节点和固定服务线程，hard IRQ 不执行 callback；
- UART 已拆成 control、IRQ、emergency-TX 三端点；
- Starry perf overflow 使用 owner-CPU registry、generation 和本地 IRQ grace，不获取上层睡眠锁；
- timer IRQ 仍直接调用 ax-task 的 owner-CPU deadline 服务，但该接口被限制为有界、无分配、无任意 callback；
- `IrqWaitCell` 已将任务侧 publication token 与 grace-period drain 拆成不同 move-only 类型。IRQ 完成只进入 `Draining`，任务侧 `try_finish()` 后才重新进入可复用的 `Detached`；正常生命周期不再依赖永久泄漏，也不会在 hard IRQ 中等待或析构。

USB/vsock 控制器协议属于外围驱动；除非它们违反上述调度交接契约，否则单独登记 issue。

## 审计矩阵

| 领域 | Linux 对照 | 核心不变量 | 当前结论 |
| --- | --- | --- | --- |
| placement | `try_to_wake_up()`、rq locking | 一个线程只能有一个物理 owner | 已用 `SchedulerPlacement` 状态机收敛 |
| 远程投递 | `ttwu_queue_wakelist()`、`irq_work` | payload/epoch 先于 IPI；handler 先 claim 旧投递 | 已收敛到 runtime 物理门铃与 owner inbox |
| CPU 生命周期 | `sched_cpu_deactivate()`、`sched_cpu_dying()` | 先关 placement，再 drain producer，最后 offline | `Online/Inactive/Draining/Offline` 已实现 |
| task deadline | `hrtimer` | IRQ 只处理 generation-bearing 值记录 | park/CBS/zero-lag 已类型化 |
| clockevent | `clockevents_program_event()` | 每 CPU 单一物理 owner；无期限用 `Option` | `Offline/Idle/Armed/Firing` 已实现 |
| switch tail | `finish_task_switch()` | 清 `on_cpu` 后才能回收 | baton 与可重试 tail 已实现 |
| PI | `rtmutex` | 注册、deboost、grant 同一事务；锁外 wake | generation-bearing PI 已实现 |
| IRQ waiter | `__free_irq()`、`synchronize_irq()` | 撤销后 grace，再释放 | token/drain 类型状态与同地址 ABA 防护已实现 |
| signal | `recalc_sigpending_tsk()` | scan 后只能确认已观察 generation | 单调 interruption generation 已实现 |
| process | `exit_notify()`、`do_wait()` | 单一 PID generation 与关系锁序 | 采用 dev `ProcessIdentity` |
| perf | `perf_install_in_context()` | task 与 CPU target 分离；owner CPU teardown | 已类型化并有 IRQ grace |
| tracepoint | RCU/SRCU probe arrays | 完整 generation 先发布；读侧 grace 后释放 | 快照与 epoch reclaimer 已实现 |
| 通用 timer | threaded hrtimer/task work | 任意 callback 不进 hard IRQ | Starry/AxVM worker 独立于 ax-task |
| serial | serial core/PREEMPT_RT handler | IRQ 只 ACK/drain/publish | 三 endpoint 已实现 |
| architecture idle | 各架构 idle entry | pending recheck 与 idle 原子提交 | 四架构已有注入测试 |

## 调度与远程投递

### placement

`SchedulerPlacement` 是唯一 placement 状态源：

```text
Detached
Queued(cpu)
Running(cpu)
SwitchingOut(cpu)
Migrating(from, to)
ExitedAwaitingTail(cpu)
```

enqueue、dequeue、wake、migration、switch-tail 和回收只能通过状态转换方法。blocked thread 的历史 wake route 不是物理 owner；CPU offline 时可重定向到仍 online 的 carrier。

### owner-CPU 与 runqueue

`CpuLocal` 为固定、不可移动的 per-CPU owner，内部拆为：

- `OwnerDispatchState`：current/idle、runqueue、RT/Fair、switch handoff；
- `DeadlineClassState`：Deadline admission、GRUB/CBS；
- `LocalTaskDeadlineState`：deadline heap、expired buffer、物理 deadline 发布；
- `OwnerDrainScratch`：远程 wake 与 control 的预分配 drain buffer。

公开 owner 操作必须证明持有 runtime IRQ pin 或 scheduler baton。仅有一个嵌套的 `NoPreempt`/锁 guard 不构成 runqueue ownership。

### 物理门铃

ax-task 只发布逻辑 sticky work 和 payload。ax-runtime 的 `SchedulerIpiDoorbell` 是唯一物理 coalescer：

1. producer 发布 inbox/payload；
2. 设置 sticky/epoch；
3. 必要时发送 IPI；
4. handler 入口先消费旧 doorbell；
5. drain 后若发现更新的 epoch 或 remainder，再发新门铃。

idle polling 与 work pending 共用原子状态，确保“观察到 polling 就省略 IPI”和“退出 polling 后新工作一定有物理边”之间没有丢唤醒窗口。

## Task deadline 与 clockevent

`ax-task::TaskDeadlineQueue` 只接受：

- sleep/park/wait timeout；
- RR/Fair/Deadline 调度期限；
- ax-task 自身 deferred task-work deadline。

条目只保存 `ThreadId`、generation、typed kind 和有限 deadline，不保存闭包、OS 对象或驱动对象。rearm 必须物理替换旧节点，cancel 必须物理移除；不得以 tombstone 占容量。

`ax-runtime::LocalClockEvent` 是以下状态的唯一 owner：

```text
Offline | Idle | Armed(deadline) | Firing
```

timer IRQ 顺序：

1. platform claim/ACK；
2. `Armed -> Firing`，旧 arm 立即失效；
3. 更新 periodic source；
4. 调用有界 `on_clock_event(now, budget)`；
5. 发布 sticky deadline work / need-resched；
6. 合并 task deadline 与 periodic tick，统一编程一次；
7. 返回平台做 EOI。

无期限用 `Option<MonotonicDeadline>`，不能用 `u64::MAX` 直接下发硬件。ns 到 tick 使用向上取整和饱和转换；已过期值钳制到设备最小非零 delta。

物理 timer 是加速路径，不是唯一正确性来源。scheduler safe point 会有界提升已经过期的 task deadline，避免丢失或过晚的硬件边永久挂起 sleeper。

## PI、等待与锁边界

### PI mutex

ax-sync 与 ax-task 的 PI handoff 遵循 Linux rtmutex 的事务边界：

1. 在 metadata 临界区验证 waiter、owner、generation 和 donation chain；
2. 发布新 owner 与 deboost/grant；
3. 释放 raw metadata gate；
4. 最后唤醒选中的 waiter。

`PiLockId` 与 waiter registration 均带 generation，锁销毁前必须 quiesce，防止地址复用 ABA。任务等待通过 park/completion 睡眠，不在禁抢占区做无界 spin。

### 锁选择

- worker 独占数据：不加锁；
- 纯任务态共享且可能分配/等待：PiMutex；
- scheduler runqueue、hard IRQ、panic：有界 raw gate/原子/固定队列；
- 任务态 registry：PiMutex，而不是可被持锁任务抢占的裸 spin lock；
- wake 和 callback 始终在广域锁外执行。

`scope-local` 底层只提供 bounded lease/`try_*`，Starry 在外层以 PiMutex 串行化任务态 mutation。migration disable 只保证任务不迁移，不允许同一 task-local owner 同时在两 CPU 激活。

## Starry 生命周期与 perf

### PID/zombie

`ProcessIdentity` 是 PID 可见性与 reap 的唯一状态机。parent/children/process-group 更新通过稳定 PID/PGID 排序的关系事务完成，避免 reparent 与 retire 锁序反转。

### 用户等待与信号

`block_on_user` 的终态为：

```text
Ready | Interrupted | TimedOut
```

operation ready 优先，其次 signal，最后 timeout。信号 publication 使用单调 generation；consumer 只能 ack 扫描前快照，不能清掉并发到达的新 signal。

### perf

`PerfTarget` 区分：

- `pid == 0`：当前 task context；
- `pid > 0`：指定 task；
- `pid == -1 && cpu >= 0`：指定 CPU；
- 其他组合按 Linux 返回错误。

CPU PMU configure/enable/disable/read/unregister 在固定 owner-CPU worker 执行。overflow registry 保存强引用和 generation，不保存可释放的 wake 裸指针。关闭顺序为 mask counter、撤销 slot、完成本地 IRQ grace、再释放输出对象。

## 驱动 IRQ 边界

### UART

`SerialParts` 分为：

- control/task endpoint；
- hard-IRQ endpoint；
- emergency-TX endpoint。

IRQ 只做 status、ACK/mask、有界 FIFO drain、值入队和 signal。普通 port 由 worker 独占。panic TX 使用非阻塞 register gate，竞争时丢弃，不等待被中断的 owner。

### USB 与 vsock

USB hard IRQ 按 acknowledge/mask、task drain、rearm 三阶段处理。xHCI 的 IMAN/ERDP gate 仍是硬件协议的一部分，不能机械替换成睡眠 mutex。

vsock hard/poll 路径只发布固定事件与 credit snapshot，connection manager 和 socket wake 在释放设备 gate 后由 worker 处理。第三方 manager 吞掉 `CREDIT_REQUEST` 细节的问题由 issue #1724 跟踪。

## 主要确定性红绿证据

| 缺陷 | 修复前确定性表现 | 修复后不变量 |
| --- | --- | --- |
| AArch64 guard 所有权 | task B 继承 task A 的 depth；QEMU panic `unbalanced CPU-local preemption guard exit` | load/store 架构 depth 跟随 `CurrentThreadHeader` |
| current 指针校验 | 错位 publication 返回 `Ok(0x1)` | guard 访问前返回 `CurrentThreadMismatch` |
| remote affinity | completion 在目标 owner 真正 enqueue 前完成 | generation completion 在 destination commit 后发布 |
| clockevent 丢边 | overdue sleeper 永久挂起 | scheduler safe point 有界恢复 overdue deadline |
| PI 地址复用 | 新锁可匹配旧 donation edge | `PiLockIdentity` generation 永不复用 |
| IRQ waiter | 第二次 IRQ 可被注册尾清掉 | 单原子状态线性化 Pending/Waiter/Notifying |
| IRQ registration ABA | 旧 detach 在 generation 检查后暂停，IRQ 完成并以同地址发布新 generation；恢复后旧 CAS 删除新 waiter 并 panic | IRQ 完成进入 `Draining`；旧 token 完成 grace 前同地址节点不可 rearm |
| signal ack | scan 后并发 SIGKILL 被 boolean clear 擦除 | generation ack 不越过新 publication |
| perf migration | 旧 CPU slot 留下 stale wake pointer | owner-CPU teardown + registry generation + grace |
| CPU timer | reader等待已被抢占 writer，系统 livelock | owner-only vtime writer + 原子 group aggregate |
| clone publication | PID/TID 可见后 placement 失败再回滚 | stage scheduler first，identity commit 后只做 infallible activate |
| futex wake | syscall 每次 wake 后额外 yield | wake publication 自己驱动 reschedule |
| Deadline scan | 每次 schedule 扫描无关 reservation | typed timer node 和 owner heap |

bug fix 必须先证明旧实现稳定失败，再用同一测试证明修复。纯文件移动和可见性收敛阶段例外。

## 性能审计

性能结论不能仅靠 QEMU plugin 样本，也不能只看 DHCP。当前证据分两类：

- qperf 指令插件曾显示 scheduler safe point、owner inbox drain 和 policy path 放大；插件本身改变 TCG 调度，因此只用于定位热点；
- 同配置 x86_64 full-system common cases 曾观察 ext4 inode、page-cache、SMP futex wake-op、AVX forced-switch 明显慢于基线，而网络 dataplane 接近，说明问题集中在高频 wake/yield/多 CPU runnable 路径。

已经完成的通用优化包括：

- 空 owner inbox 不进入 drain；
- current-CPU reschedule 不走通用远程 registry；
- forced yield 不顺带 drain 无关 deadline/task-work；
- unchanged semantic task deadline 不重复发布物理 clockevent；
- futex wake 不额外强制 yield；
- task-only scheduler metadata 使用 preempt-only ticket lock，不扩大 IRQ-off 区间。

性能接受必须用同一 workload、同一 QEMU 参数、正式 success marker 对比 Linux RT 或已确认基线；发现慢于基线即可中止全量并缩小到目标 case，用 qperf/GDB 检查 wake、lock、IPI 和 safe-point 调用链。

## 模块化结果

- `TaskSystem` orchestration 只负责编排，registry/reap、placement、owner scheduling、deadline、PI、balance、deferred work 分模块；
- `CpuLocal` 按 dispatch、deadline、inbox、switch-tail、snapshot 划分；
- `ThreadSchedState` 按 lifecycle、policy、placement、deadline、PI、runtime binding 划分；
- ax-runtime task 按 bootstrap、thread resources、context/switch、executor、IPI、clockevent、guard-state 划分；
- Starry perf、process identity、lifecycle/wait、sampling registry 分模块；
- serial 按 control、IRQ、emergency-TX 和 worker ownership 分模块。

模块拆分不得增加第二状态源，也不能用 facade mirror 复制 owner 状态。

## 当前未关闭项与范围外问题

### 本轮必须继续处理

- 完成四架构 current-head build/QEMU 与 CI terminal 结果；
- 对新 dev 合入的 AxVM CPU_ON/CPU_OFF/reset 生命周期保持 `TaskHandle` 适配；
- 继续检查高频 wake/yield workload 的 scheduler-work amplification。

### 单独 issue，不在本轮顺手修改

- #1724：virtio-vsock manager 的 credit observer/capacity 边界；
- #1767：Starry system 大组超时、缺少逐 case 持久 timing；
- #1772：Starry ktest 错误启用 `log/std` 的 feature graph；
- #1773：Starry target-aware clippy 永久 CI 能力；
- USB 控制器、USBFS、第三方 connection manager 的外围协议缺陷，除非它们破坏调度交接契约。

## 验证策略

### 最低层

- `cargo test -p cpu-local --features host-test`；
- `cargo test -p ax-task` 和 loom；
- ax-runtime guard/clockevent/context virtual-runtime 测试；
- ax-sync PI/loom；
- Starry process/perf/signal 定向测试；
- UART IRQ 无分配、无阻塞和生命周期测试。

### clippy 与静态检查

- `cargo xtask clippy --package cpu-local`；
- `cargo xtask clippy --package ax-task`；
- `cargo xtask clippy --package ax-runtime`；
- `cargo xtask clippy --package axvm`；
- Starry/ax-sync/ax-net 对应 feature matrix；
- `cargo fmt --all --check`；
- `git diff --check`；
- 禁止 hard IRQ 路径出现任意 callback、分配、睡眠锁和上层裸 wake 指针。

### QEMU 里程碑

- 四架构 ArceOS `rust/all`；
- Starry 先跑 sched、timer、futex、perf、clone/exec、pidfd/waitid 定向 case；
- 再串行跑四架构 `qemu/system`；
- 只认可 runner 的正式成功标志；queued、cancelled、部分完成都不算通过；
- hang 先缩小到 grouped subcase，必要时用 GDB 检查 timer begin/finish、IPI consume、idle commit、switch tail、guard depth 和 IRQ endpoint grace。

## 完成规则

每个阶段必须满足：

1. finding 有明确所有权与 Linux 对照；
2. bug 有确定性红绿测试；
3. 目标 crate test/clippy 通过；
4. 格式和 `git diff --check` 干净；
5. 阶段提交并推送检查点；
6. PR 描述同步问题、修改、设计依据、红绿证据和当前未完成项。

最终完成要求：审计矩阵内无未处理的任务调度 finding，正常 IRQ 生命周期不依赖裸指针或永久泄漏兜底，四架构 QEMU 与 GitHub CI terminal 全绿。显式 `mem::forget` 等协议破坏仍以泄漏而非 UAF 失效。范围外问题必须有可接续的 issue 证据，而不是通过跳过或放宽测试隐藏。
