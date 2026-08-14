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
- generation-bearing 调度身份不得直接当作 Linux TID/PID。

内部路径不保留兼容别名。axstd 的任务扩展统一从
`ax_std::os::arceos::task` 进入；旧 `modules::ax_task` 已删除，仓内消费者直接迁移。

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
只参考 v7.1 当前主路径；为旧内核、旧平台模型或本分支旧 API 保留的 compatibility
分支、fallback 和 deprecated wrapper 一律不引入。

## 四架构统一模型

### 统一的是语义，不是物理存储

四架构应共享同一组前端语义：

1. 普通 `preempt_count` 描述当前执行上下文的嵌套深度；
2. `need_resched` 是在下一安全点进入调度器的单调请求；
3. hard-IRQ/IRQ mask、scheduler baton、runqueue owner 属于 CPU；
4. 普通 guard 的最后一层只能原子转换为 scheduler baton，不能先暴露可抢占窗口；
5. 上下文切换尾在清除前一线程物理 `on_cpu` 后，才能迁移或回收资源；
6. current-task 发布先提交 CPU anchor；具有独立 current 寄存器的架构再由裸切换尾更新任务寄存器。任务寄存器与 kernel TLS 寄存器重叠的后端以 anchor 为 current 权威。提交与裸切换之间禁止执行可失败或会释放所有权的 Rust 代码。

物理存储由架构后端决定：

| 架构 | Linux current 来源 | Linux 普通抢占状态 | TGOSKits 后端 |
| --- | --- | --- | --- |
| x86_64 | per-CPU `current_task`，通过 GS 访问 | per-CPU `__preempt_count`，倒置的 resched 位折叠在同一字 | `CpuRuntimeAnchor` 的 GS 固定偏移 |
| AArch64 | `SP_EL0` 指向 current task | `current_thread_info()->preempt`，count 与 need-resched 均跟随任务 | `CurrentThreadHeader`，裸切换尾写 `SP_EL0` |
| RISC-V | `tp` 指向 current/task thread-info | 通用 task-owned `preempt_count`，resched 使用任务 TIF | 无 kernel TLS 时裸切换尾写 `tp`；TLS 占用 `tp` 时 CPU anchor 发布同一 `CurrentThreadHeader` |
| LoongArch64 | `$tp` 指向 current thread-info | 通用 task-owned `preempt_count`，resched 使用任务 TIF | 无 kernel TLS 时裸切换尾写 `$tp`；TLS 占用 `$tp` 时 CPU anchor 发布同一 `CurrentThreadHeader` |

因此不采用“四架构强制同一内存位置”的模型。统一接口为 `scheduler_*preempt*`，后端选择 live word。x86 保留 GS 快路径；其余 load/store 架构让 guard 状态随任务迁移。scheduler baton 和 IRQ owner 在四架构都保持 per-CPU。

### current-thread 安全边界

`scheduler_current_thread()` 用来在尚未构造 guard 时读取 current，因此不能先依赖普通 `CpuPin`。它必须：

- 拒绝零地址和错误对齐；
- 只有 task/current 寄存器确实与 kernel TLS 重叠的 CPU-anchor 模式才重读 CPU base，迁移则重试；
- CPU-anchor 模式校验 header 仍绑定到采样的 CPU area；
- 只返回非逃逸借用，调用方不得跨上下文切换保存裸指针。

`PreparedThreadSwitch::commit()` 只发布 runtime anchor。AArch64 无论是否启用 TLS 都在裸切换尾写 `SP_EL0`；RISC-V/LoongArch 仅在 task register 未承载 kernel TLS 时写 `tp`；x86 的 GS 直接读取 anchor。具有独立 current 寄存器的后端漏掉第二阶段会由 `CurrentThreadMismatch` 检出。

### AArch64 current、TLS 与抢占状态的根因修复

CI run `31025836159` 的 AArch64 ArceOS QEMU 在并行内存分配完成后进入普通
preemption guard，因 `CurrentThreadMismatch` panic。相同完整命令在修复前本地也能通过，
说明 QEMU 结果只能证明低概率窗口存在，不能定位所有权错误。确定性红测
`aarch64_current_register_is_independent_of_kernel_tls` 则直接验证架构能力：旧实现把
Cargo `tls` feature 当成全架构 current 模式开关，错误地为 AArch64 选择 CPU anchor，
而不是与 kernel TLS 独立的架构 current 来源。

Linux v7.1 的对应所有权是：

- `arch/arm64/include/asm/current.h` 始终从 `SP_EL0` 取得 current；
- `arch/arm64/kernel/entry.S::cpu_switch_to` 在裸切换尾写入下一任务指针；
- `arch/arm64/include/asm/preempt.h` 从 current thread-info 访问任务自有的
  `preempt.count/need_resched`；
- `TPIDR_EL0` 是独立 TLS 寄存器，不改变上述 current 所有权。

因此 `cpu-local` 现在声明的是架构能力 `current_source_aliases_kernel_tls`，而不是由 feature
选择一套全局 ABI。AArch64 与 x86_64 为 `false`；RISC-V 与 LoongArch64 因 `tp` 同时承担
kernel TLS 才为 `true`。host 测试按 x86 GS/FS 分离模型运行，不再用 TLS feature 人为切换
current 来源。AArch64 TLS 裸切换在同一不可失败汇编尾同时恢复 `TPIDR_EL0`、内核栈和
`SP_EL0`，随后才返回下一任务；CPU anchor 仍由 switch commit 先发布，供 userspace trap
入口在 `SP_EL0` 暂存用户栈时恢复 current，但不成为第二个可独立更新的 current 权威。

修复后 `cpu-local` 的 host 与 host+TLS 契约测试均通过，`ax-cpu` 四架构 feature clippy
通过；AArch64 `task-tls` 及连续三次完整 Rust QEMU 均取得正式通过标志。三次完整 Rust
QEMU case 分别为 4.10、4.74、4.25 秒；这些时间只记录本次稳定性验证，不作为 Linux RT
性能对比结论。

### CPU-owner per-CPU 安全边界

`scheduler_current_thread()` 只服务 task-owned 状态。IRQ depth、scheduler baton、CPU remote endpoint
等物理 CPU 状态不得先经 current-task header 再反查 CPU area。Linux v7.1 也将
runqueue 与 IRQ/context 状态通过 `raw_cpu_ptr()`/`this_cpu_ptr()` 选择；x86
`__preempt_count` 直接使用 GS per-CPU 操作，不依赖 `current_task` 反查。

本分支的内部 `with_scheduler_cpu[_mut]()` 因此只从架构 CPU-area 来源构造
非逃逸 `SchedulerCpuArea` token，不经 task header，也不在每次 guard 访问时
重建并重验完整 `CpuAreaRef`。HRTB callback 禁止 token 逃逸。它不替代常规
`CpuPin`：调用者必须已经保证不迁移/不切换，可变访问还必须排除本地
IRQ 重入和远程别名。RISC-V LinuxCurrent 模式的 `tp` 同时是 current header，
因此该后端仍可从 header 导出 area；这是物理寄存器差异，不改变前端
“访问 CPU-owned 状态”的单一语义。host 确定性回归对每次 CPU-owner 选择要求
`cpu_base=1, current_thread=0, initialized_area_validations=0`，防止
x86/AArch64/LoongArch 退回 task-header 绕行或重复 area bootstrap/identity 验证。

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

- scheduler IPI 使用 `published_epoch/claimed_epoch/edge_armed`，handler 入口先 claim
  已发布 generation，再允许并发 producer 建立新物理边；
- deferred task-work 使用 intrusive 节点、generation claim 和固定服务线程，hard IRQ
  不执行 callback；
- UART 已拆成 control、IRQ、emergency-TX 三端点；
- Starry perf overflow 使用 owner-CPU registry、generation 和本地 IRQ grace，不获取上层睡眠锁；
- timer IRQ 仍直接调用 ax-task 的 owner-CPU deadline 服务，但该接口被限制为有界、无分配、无任意 callback；
- `IrqWaitCell` 已将任务侧 publication token 与 grace-period drain 拆成不同 move-only 类型。cell、token、drain 分别持有真实 owning reference；IRQ 完成只进入 `Draining`，任务侧 `try_finish()` 后才重新进入可复用的 `Detached`。正常生命周期不再依赖 destructor leak，hard IRQ 的 notify 路径保持零分配、零释放。

### IrqWaitCell publication owner 与 grace period

Linux v7.1 的 `__free_irq()` 先在 `desc->lock` 下从 action chain 撤销 handler，并在最后一个
action 时 shutdown IRQ；随后释放 handler 可能需要的 bus/raw lock，再经
`__synchronize_irq()` 等待 `IRQD_IRQ_INPROGRESS` 与 `threads_active` 清零，最后停止 threaded
handler、deactivate irq domain 并释放 action。`synchronize_irq()` 明确只能从可抢占任务上下文
调用，且调用方不能持有 handler 需要的资源。这一顺序把“关闭新 reader”“等待已有 reader”
和“释放 payload”分成三个 owner 清晰的阶段。

旧 `IrqWaitRegistration` 只拥有一个 `Pin<Box<_>>`，cell 的原子槽保存不计所有权的裸地址。
registration 在 token 未完成时析构只能 debug panic，并在 release build 永久泄漏 allocation；
这不是 grace-period owner，而是依赖析构分支阻止 UAF。当前实现改为：

1. `register()` 在任务上下文预先创建 cell-owned `Arc` reference，再以 raw Arc 地址发布；
2. token 独立持有同一 node，保证 notifier 执行期间 wake payload 仍有任务侧 owner；
3. notify 成功 claim 后一次性收回 cell reference，执行固定 `ThreadWakeHandle`，将 generation
   从 `Notifying` 发布到 `Draining`；正常 typed 路径中 token/registration 仍持有 reference，
   因而 hard IRQ 只做 refcount decrement，不触发 allocation 析构；
4. `detach()` 撤销尚未 claim 的 publication，或让 drain 等待已 claim reader；`try_finish()`
   只在观察到 `Draining` 后推进到可复用的 `Detached`；
5. cell 自身在任务上下文销毁时，以独占访问撤销仍 attached 的 generation，再释放 cell owner。

公开 API 不再把 registration 借用期当作唯一内存安全条件：即使任务态 handle 在 notifier
完成前释放，token/drain 与 cell 的 owning reference 仍保持 node 和 wake payload 有效。显式
`mem::forget(token)` 会像所有 Rust owning token 一样泄漏它自己的 reference，但代码不再有
“检测未 quiesce 后主动泄漏 Box”的 fallback 分支。

确定性红测 `cell_owns_a_published_node_after_its_token_is_abandoned` 在旧实现稳定命中
`dropped before token quiescence`；新实现中 registration 正常析构后，cell 仍能安全完成已
发布 wake。补充行为测试覆盖 cell teardown 后同一 registration rearm，以及 registration 在
direct wake 阻塞期间释放、drain 必须等 wake 返回。`hard_irq_contract_is_zero_alloc_zero_free_and_zero_poll`
现已把 `IrqWaitCell::notify()` 纳入 allocator audit，要求 hard IRQ 中 allocation/deallocation
均为零。

USB/vsock 控制器协议属于外围驱动；除非它们违反上述调度交接契约，否则单独登记 issue。

## 审计矩阵

| 领域 | Linux 对照 | 核心不变量 | 当前结论 |
| --- | --- | --- | --- |
| placement | `try_to_wake_up()`、rq locking | rq 独占 queued/running，CPU switch baton 独占 outgoing stack | 已删除 `target_cpu` CPU 身份双真相和 task 级 `SwitchingOut/ExitedAwaitingTail`；线程只保存最终 rq/migration placement 与独立 `on_cpu` 发布位，outgoing stack 生命周期唯一归 per-CPU `SwitchHandoff` |
| Fair/EEVDF | `avg_vruntime()`、`place_entity()`、`pick_eevdf()` | 唯一加权平均 V；sleep/migration 保存 `vlag`；eligible current 保留请求保护 | 已删除旧 wakeup granularity 与重复单调 V，迁移使用 detach 事务 |
| RT/DL 选核 | `cpupri`、`cpudl`、`rto_mask/rto_count`、`dlo_mask/dlo_count`、`RT_PUSH_IPI` | 优先级与 load 正交；DL runnable CPU 属于 cpupri HIGHER；只有可迁移候选才能发布 overload；RT 与 DL 各自维护 root-domain push iterator | root-domain cpupri/cpudl 已接入 wake placement；cpupri 包含 101 个桶，DL current/queued 发布 HIGHER；pushable membership 在 rq 事务内直接发布 RT/DL overload mask/count；两类 priority-drop 各自通过 generation-bearing iterator 串行通知 owner，不广播 IPI，也不跨类选择候选 |
| 远程投递 | `try_to_wake_up()`、`ttwu_queue()`、`resched_curr()`、`irq_work_claim()` | PREEMPT_RT 关闭 `TTWU_QUEUE`，waker 直接锁目标 rq 激活；仅实际需要抢占时发送 reschedule IPI；IPI claim 必须先于 callback/drain | 已删除 task-level remote-wake inbox；migration/control 继续使用 typed owner inbox；runtime 门铃改为 generation + physical edge ownership，coalescing 只返回成功，不再把 `Busy` 暴露为模糊的 transport 状态 |
| CPU 生命周期 | `sched_cpu_deactivate()`、`dl_bw_deactivate()`、`sched_cpu_dying()` | 先验证剩余 Deadline capacity，再关 placement、drain producer，最后 offline | `Online/Inactive/Draining/Offline` 已实现；Deadline 过量预留会在 topology mutation 前拒绝 CPU down |
| active mm | `exit_mm()`、`context_switch()`、`enter_lazy_tlb()`、`finish_task_switch()` | task-mm 所有权在 zombie 前解除；lazy CPU carrier 与页表根存储保留到最后 CPU lease | move-only token + task detach + per-CPU active lease 已实现 |
| 用户 TLB 回收 | `mmu_gather`、`mm_cpumask()`、各架构 `flush_tlb_mm_range()` | 先改 PTE，再同步 active-mm CPU，最后释放物理所有权 | 共享 mm CPU tracker + typed gather 已实现；调度 token 不再各自维护错误的 CPU footprint |
| task deadline | `hrtimer`、`sched/deadline.c` | IRQ 只处理 generation-bearing 值记录；CBS 状态机是唯一期限真值 | Deadline 已改为 class-owned 有序 AVL rq，并在节点内增广最早 CBS 事件；pick/dequeue/rekey 为 O(log n)；CBS 记账与物理入队同属目标 rq 事务；CBS 生命周期改为互斥状态，删除 `base_deadline` 镜像 |
| clockevent/nohz | `clockevents_program_event()`、`clockevents_shutdown()`、`hrtimer_interrupt()`、`tick_nohz_idle_enter()`、`tick_nohz_stop_tick()`、`tick_nohz_idle_exit()` | 每 CPU 单一物理 owner；CPU 生命周期与 firing 带 epoch；任何已投递的硬件边必须先失效旧 arm，再由逻辑时钟判断是否到期；idle 无调度事件时停止 tick；无期限用 `Option` | scheduler tick 建模为 `Running/Stopped`；online/offline 推进 CPU epoch；有效 `Armed` edge 都取得 move-only firing token，先失效旧 arm，再有界运行 hard queue/发布 soft work，早到或旧 pending edge 在 finish 时只按当前最早期限重编程一次；idle IRQ-off 提交时撤销 tick，只保留 task deadline |
| switch tail | `finish_task_switch()` | 清 outgoing `on_cpu` 后才能回收；已提交的 raw switch 不可重试 | move-only `SwitchHandoff` 同时携带 outgoing 与 incoming 的权威引用；普通 tail 只校验原子 placement、完成 runtime tail 并 release-clear `on_cpu`，不重开 rq；只有已提交 migration 的 Deadline bandwidth 转移进入 rq 慢路径；`on_switch_in` 位于 handoff consume 之后 |
| 架构 current/preempt | `current.h`、`preempt.h`、`cpu_switch_to`、`finish_task_switch()` | current 与普通 preempt 状态必须由架构唯一来源取得；TLS 只在物理寄存器确实重叠时改变 current 读取路径；裸切换尾不可失败 | AArch64 始终以 `SP_EL0` 为 current、`TPIDR_EL0` 为 TLS；x86 以 GS/FS 分离；RISC-V/LoongArch TLS 模式才从 CPU anchor 取得 current；删除全局 TLS current 模式选择 |
| PI/锁 | `rtmutex`、`spinlock_rt.c`、`wake_q` | raw rq/IRQ gate、sleeping PI、task-local pin 四层分离；每把锁拥有有序 waiter tree，owner 只接收各锁 top waiter；解锁核心在同一 preempt-disabled 事务内选择 waiter、deboost、发布 ownerless 状态并加入 wake_q，释放元数据锁后由核心完成 wake | `PiMutexCore` 唯一拥有 generation-bearing owner word 与 allocation-free AVL waiter tree；线程预备 lock/owner 两套 linkage，owner donor tree 只保存每把已持有锁的 top waiter；ax-sync 不再保存第二份 owner、selected、waiter 容器或可丢弃 wake handle；registration、release、claim 与 release 后 wake 均由 ax-task 持有完整事务，外层不能遗漏 handoff wake |
| 阻塞等待 | `do_lock_file_wait()`、`wait_event_interruptible()`、`locks_delete_block()` | wake 只是重试提示；条件与临时阻塞关系由领域层拥有，返回前必须先注销 | scheduler notification 与 nofault access retry 已类型化分离，fcntl/futex 外层负责重试 |
| IRQ waiter | `__free_irq()`、`__synchronize_irq()`、`synchronize_irq()` | 先关闭新 reader，再等待 hard/threaded reader grace，最后在任务上下文释放；hard IRQ 不析构 payload | cell/token/drain 持有真实 owning reference；发布槽的 raw Arc reference 只在原子 claim 后回收；registration 可独立释放；generation 防 ABA；删除 destructor-leak fallback，并以 allocator audit 证明正常 notify 零分配零释放 |
| signal | `recalc_sigpending_tsk()` | scan 后只能确认已观察 generation | 单调 interruption generation 已实现 |
| process | `exit_notify()`、`__do_wait()`、`do_wait_thread()`、`ptrace_do_wait()` | 单一 PID generation 与关系锁序；每次 wake 后重扫 children/ptrace/zombie 权威关系，禁止跨阻塞保存候选快照 | 采用 dev `ProcessIdentity`；waitpid/waitid 共用 refreshable scan，初次无候选返回 `ECHILD`，阻塞后的每次 poll 重新采集 |
| perf | `perf_install_in_context()` | task 与 CPU target 分离；owner CPU teardown | 已类型化并有 IRQ grace |
| tracepoint | RCU/SRCU probe arrays | 完整 generation 先发布；读侧 grace 后释放 | 快照与 epoch reclaimer 已实现 |
| 通用 timer | threaded hrtimer/task work | 任意 callback 不进 hard IRQ | Starry/AxVM worker 独立于 ax-task |
| serial | serial core/PREEMPT_RT handler | IRQ 只 ACK/drain/publish | 三 endpoint 已实现 |
| architecture idle | `do_idle()`、各架构 idle entry | IRQ-off final recheck、nohz enter/exit 与原子等待是一个 CPU-local 事务 | polling 撤销后由 runtime 在同一 IRQ-off 窗口处理 overdue、停 tick、复查 pending 并进入四架构原子 wait；无 resched 的 IRQ 返回后保持 tick stopped |

## 二次全量审计后的目标架构

2026-08-04 对当前分支相对 `origin/dev` 的全部调度改动再次逐链路核对 Linux
v7.1 PREEMPT_RT。此次不再把“已有枚举/guard/缓存”当作完成标准，而以状态事实是否只有
一个 owner、锁是否对应真实执行上下文、失败是否能在同一事务中回滚作为验收标准。

下列结构必须作为一个目标模型落地，不保留旧新双轨、兼容 facade 或回退状态：

1. **rq 是物理 placement 的唯一 owner**。线程只保存 affinity、policy、生命周期与
   `on_cpu` 发布位；queued/running 由目标 rq 及 `rq->curr` 表示。`Migrating` 是源 rq
   dequeue 到目标 rq enqueue 之间唯一允许的 carrier。`SwitchingOut` 和
   `ExitedAwaitingTail` 不再写入线程 placement。
2. **switch 暂态只在 CPU 上存在**。`SwitchHandoff` 持有 outgoing/incoming thread 与可选的
   migration publication lease。raw switch 前由同一个 rq 选择事务一次 stage，目标 continuation
   的 switch tail 一次 consume；因此普通 tail 不需要再次读取 `rq->curr`。只有它能清 `on_cpu`、
   发布迁移或允许回收。
3. **调度类通过共同 rq 边界组合**。class rank 固定为 Deadline、RT、Fair、Idle；各类
   自己拥有 enqueue/dequeue/pick/tick/yield 后端。Deadline 使用有序 rq 和 CBS
   replenishment 状态机，删除 O(n) `Vec` 扫描、`replenish_pending` 手工轮询和
   `u64::MAX` 无期限哨兵。
4. **wait/wake 遵循 state-condition-rq 事务**。waiter 在领域 bucket 锁内发布 park
   generation，wake 在同一锁内选择胜者并放入 task-embedded wake batch，释放领域锁后
   才进入 rq。不得把 callback、分配或上层对象带进 rq/IRQ gate。
5. **futex private identity 属于 mm**。`CLONE_VM` 共享 `MmIdentity + FutexDomain`；fork
   创建新 domain；exec 原子替换 mm，并让旧 domain 随旧 waiters 延迟销毁。private key
   为 `(mm generation, address)`，shared key 为稳定对象 identity 与 offset；固定 bucket
   是唯一排队锁，删除 `ProcessData::futex_table`、动态每 key table 和 teardown
   `try_lock` fallback。
6. **锁按上下文分四层**：rq raw IRQ lock、设备/IRQ raw gate、sleeping PiMutex、
   task-local CPU/preempt lease。跨 CPU 可竞争的 scheduler state 不得使用
   `PreemptTicketLock` 无界等待；PI metadata 中完成 owner generation、deboost、grant
   和 wake-batch 选择，锁外 wake。
7. **clockevent 与 nohz 是同一个带 CPU epoch 的 CPU-local 状态机**。physical clockevent 只合并 typed
   task deadline 与处于非 idle 状态时的 scheduler tick。`SchedulerTickState` 在 IRQ-off
   final recheck 中从 `Running` 转成 `Stopped` 并保存恢复相位；普通 IRQ 不产生 runnable
   work 时保持 stopped，idle exit/过期恢复只重算一次。无 work 时不得保留永久 periodic
   source，也不得用调用栈外的第二份布尔状态镜像 NOHZ 生命周期。online/offline 都推进
   epoch；`Firing` 必须持有 move-only epoch token，旧 CPU 周期的 completion 不能提交到
   新周期。任何有效 `Armed` edge 都先失效旧 arm 并进入一次有界 scheduler pass；早到或
   旧 pending edge 不产生逻辑 expiry，finish 只按当前 source state 重编程一次。
8. **scheduler entry、active-mm 与 switch plan 一次提交**。typed entry token 独占
   scheduler baton；next active-mm lease/root/current publication 在 raw switch 前准备，
   previous lease 与资源仅在 switch tail 释放。四架构只允许汇编 idle/switch 细节不同，
   Rust 状态机、锁序和生命周期一致。

删除顺序由依赖关系决定，但每个阶段必须直接切断旧入口；禁止用 feature flag、兼容别名、
双写字段或“先更新旧字段再同步新字段”的方式过渡。当前两条确定性架构红测为：

- aarch64 `task-affinity`：当前 head 在 affinity 已成功返回后仍可观察到 CPU 3，而 mask
  只允许 CPU 2；修复必须证明 owner rq、switch handoff 和 CPU-local publication 的事务；
- Starry `clone(CLONE_VM | SIGCHLD)` + `FUTEX_PRIVATE`：父进程 wake 找不到同 mm 子进程
  waiter；修复必须来自 mm-owned futex domain，而不是跨 `ProcessData` 搜索或 fallback。

### 调度类 runqueue 落地状态

顶层 `RunQueue` 现在只负责线程 membership、class 顺序和公共 placement 事务，不再直接
实现各类选择算法：

- Deadline class 使用按 `(absolute deadline, enqueue sequence, ThreadId)` 排序的 owner-local
  AVL；节点同时增广子树最早 scheduler event，等价于 Linux `dl_rq.root` 的 cached
  leftmost 与 class deadline cache。Fair、RT、Deadline 三类链接都在 `ThreadCore` 构造时
  预备，节点随线程而不是随 CPU 生存；首次 wake、迁移和 policy change 不在 rq irqsave
  临界区分配或释放。generation-bearing membership 保存精确树 key，更新和迁移不再扫描
  Deadline runnable set。
- RT class 独占 99 级 FIFO/RR priority array、active bitmap 和 PI-owner bitmap。顶层通过
  `RtEligibility::{Ordinary, PiOwnerOnly}` 指明 bandwidth 状态，不再使用含义不明的 bool；
  quota 耗尽后只从 PI-owner bitmap 选择最高优先级解锁者；各优先级队列使用预备的侵入式
  FIFO 节点，不再依赖会在首次入队扩容的 `VecDeque`。
- Fair/Idle class 继续由各自增广 EEVDF AVL 拥有，顶层固定按
  `Deadline > RT > Fair > Idle` 调用 class pick。

### root-domain cpupri 与 cpudl 索引

仅有 owner-local priority array/Deadline tree 还不足以实现 Linux RT 的跨 CPU 选核。
旧实现虽然在 `CpuLoadSummary` 发布了 `current_key` 和 `pushable_key`，但 RT/Deadline 的
`wake_thread_direct` 与 `place_ready` 最终仍按 runnable count 选择 CPU；负载与优先级是
正交维度，因此较高优先级的 RT wake 可能被放到正在执行更高优先级 RT 工作的 CPU。

当前实现增加由 root domain 持有、由 runqueue 派生的两类索引：

- RT 使用 100 级 cpupri bitmap：0 表示没有 RT runnable，1..99 表示该 CPU 当前与队列中
  最高的 POSIX RT priority。rq 发布先加入新桶，再以 Release 发布 per-CPU level，最后从
  旧桶删除；读取者观察桶位后以 Acquire 重验 level，既不会把 CPU 从所有桶中短暂漏掉，也
  不会把升优先级过程中遗留的旧桶位当成有效目标。选核只扫描低于 wakee priority 的桶，
  同一桶优先 cache-hot CPU，再按 CPU ID 确定性选择；找不到更低优先级目标时才回退到原 CPU
  与普通可用 CPU 选择。
- Deadline 使用 cpudl 风格的 free-CPU set、per-CPU heap index 和以最晚 absolute deadline
  为根的 max-heap。初始 Ready placement 先选没有 DL runnable 的允许 CPU，否则像 Linux
  `cpudl_find()` 一样只读取 heap root，并且仅当该 CPU 允许且其最早 runnable deadline 晚于
  新实体 absolute deadline 时采用，非空 CPU 查询不会退化成全 CPU 线性扫描。CPU online/offline 与 rq
  summary 发布同步更新索引，索引永远只是提示；线程状态与物理 membership 仍由目标 rq
  事务最终确认。
- RT/DL enqueue 在发布摘要后若形成 overloaded pushable rq，会像 Linux
  `rt_queue_push_tasks()`/balance callback 一样立即发布 sticky owner work 和 doorbell。
  waker 不持源 rq 去获取目标 rq；owner 在锁外重新选择、重新获取线程状态并验证 queued、
  affinity、on_cpu 和 migration generation 后才提交迁移，从而避免跨 rq 锁序反转。

三条确定性红测分别证明：宽 affinity RT wake 不再按 load/cache hint 落到更紧急的 CPU；
新 DL Ready 实体选择最晚 deadline CPU；低优先级 wake 即使不触发当前任务抢占，也会为已
overloaded 的 RT rq 发布一次 owner balance doorbell。旧实现三条均稳定失败。

DL CBS/GRUB bandwidth 和 zero-lag timer 仍归精确 per-rq owner，但 blocked DL wake 与 CPU
offline 已建立 Linux `TASK_WAKING/task_rq_lock` 式迁移事务：先在源 rq 删除 CBS/zero-lag
registration、`this_bw/running_bw/member`，提交源派生索引，再在目标 rq 安装相同 generation
的带宽/activity 并发布目标 scheduler deadline。cpudl 只负责选核提示，不能单独迁移这些事实。

RT bandwidth 仍按 Linux `struct rt_rq` 保持 per-CPU 事实源；Linux 的共享
`rt_bandwidth` 负责 period timer 和 runtime balancing，并不意味着所有 CPU 共用一份
`rt_time`。当前 `RootRtBandwidth` 独占 monotonic period timer、base runtime 和
runtime-sharing 总锁，per-rq `RtRunQueueBandwidth` 独占 `rt_time/runtime` 借贷账本；
`CpuRunQueueState::rt_throttled` 是 rq eligibility 的唯一事实源。quota 边缘在 owner rq 事务内
取得嵌套 bandwidth lock，按 Linux `do_balance_runtime()` 从 span 内 donor 借取 1/N spare，
再在同一 rq 事务中发布 throttle transition。CPU offline 按 `__disable_runtime()` 贪婪收回
loan。迁移线程不携带源 CPU 的 `rt_time`，Fair/root publication 也不为读取 eligibility 进入
RT bandwidth lock。

旧实现对 128 个 Deadline runnable thread 的一次 EDF pick 稳定访问 128 个实体；确定性
红测要求访问数不随 runnable 数线性增长，新实现只访问有序树头。另一个既有迁移回滚测试
在持有 `ThreadSchedState` guard 时调用公开 `assigned_cpu()`，会再次获取同一 ticket lock
并永久自锁；测试现已在公开查询前释放内部 guard。完整串行 276 项单元测试由此能够终止，
不再用“输出完测试名字”替代 runner 退出状态。

首次 class enqueue 的分配审计在旧实现稳定观察到 3 次分配；新实现分别对 Fair、RT、
Deadline 的首次物理入队断言 allocation/deallocation 均为 0。Deadline 阻塞任务在 zero-lag
前被远程唤醒时，旧实现要等 owner inbox drain 才把 `ActiveNonContending` 改为
`ActiveContending`；现在目标 rq 锁同时提交 member、GRUB/CBS bandwidth、activity 和实体
membership，wake 返回时即满足该不变量。owner-only task deadline heap 只负责随后发布 CBS
clockevent，不再承担已经可运行实体的带宽真值。

## 调度与远程投递

### placement

目标模型中，thread placement 只保留稳定状态：

```text
Detached
Queued(cpu)
Running(cpu)
Migrating(target)
```

其中 `Queued/Running` 是 rq 所有权的观测结果，不是可独立更新的第二组事实；
`SwitchingOut/ExitedAwaitingTail` 由 per-CPU `SwitchHandoff` 表示。enqueue、dequeue、wake、
migration、switch-tail 和回收只能通过 rq 事务与 handoff 方法。blocked thread 的
`wake_cpu_hint` 只承担 Linux `wake_cpu` 式的直接唤醒偏好，不再作为公共 CPU 身份。
`ThreadHandle::assigned_cpu()` 只返回 Linux `task_cpu()` 对应的最后一次 rq assignment，
不再用 wake hint 补值。blocked task 保留旧 `task_cpu()`；CPU offline 只把独立的
`wake_cpu_hint` 重定向到仍 online 且满足 affinity 的 CPU。物理执行 owner 由
`scheduler_fence_cpu()`/`on_cpu` 单独报告，下一次唤醒偏好不能冒充 rq 或执行所有权。

### owner-CPU 与 runqueue

`CpuLocal` 为固定、不可移动的 per-CPU owner。runqueue 已从 owner-only
`OwnerDispatchState` 拆到 `CpuRemote` 的 IRQ-safe raw rq lock 下，使远程 waker
可以采用 Linux v7.1 PREEMPT_RT 的 active wake 模型：

- remotely lockable `RunQueue`：Fair/RT/Deadline 队列、runnable load、调度策略时钟、
  Deadline member 与 GRUB/CBS bandwidth 记账；使用 IRQ-safe raw rq lock，四架构共享同一
  事务模型；
- rq-owned current/idle/current dispatch：和各 class queue、`nr_running`、clock、pushable、
  PI/RT/DL bandwidth 一起由同一个 `CpuRunQueueState` raw rq lock 保护；
- IRQ-safe `CpuRemote::CpuDeadlineState`：deadline heap、expired buffer、scheduler deadline
  publication，允许 timer IRQ/soft worker 与持有 task-rq 事务的远程迁移安全交接；
- owner-only `OwnerDispatchState`：只保留 switch handoff 与调度 continuation scratch；
- `OwnerDrainScratch`：只保留 migration/control 等必须由 switch-tail owner 完成的控制消息。

公开 owner-only 操作必须证明持有 runtime IRQ pin 或 scheduler baton。runqueue
操作改为持有目标 rq raw lock；仅有一个嵌套的 `NoPreempt`/锁 guard 既不构成
owner ownership，也不能替代 rq lock。rq lock 内禁止 callback、资源释放和任意 wake。

所有生产 rq 写入都通过 `OwnerRqTxn`：构造时关闭本地 IRQ、取得目标 rq lock 并只采样一次
`rq_clock_sample`；事务内完成 common accounting、class hook、current/placement 变化与
RT/DL bandwidth；显式 `commit` 一次发布 current、load、cpupri、cpudl、rto/dlo，再释放 rq。
scheduler request 的 generation 只能在 commit 和 current publication 之后 acknowledge。
事务没有析构提交或错误兜底；未显式 commit 即触发 fatal invariant，从而禁止半事务悄悄
解锁。`sched_class` 是静态闭集，统一提供 enqueue/dequeue/check_preempt/put_prev/set_next/
task_tick/migrate hook；`RunQueue` 只负责 Linux common rq accounting 和 membership。

远程 wake 的状态事务固定为：

```text
lock thread state -> reserve stable target publication -> publish TASK_WAKING
-> wait/validate on_cpu release -> lock target rq -> validate placement -> activate
-> check preemption -> unlock rq -> optional reschedule IPI
```

`switch_handoff`、current publication、前一任务 `on_cpu` release 和资源回收继续只允许
owner CPU 执行；它不替 waker 重开 task/rq 事务。目标 publication lease 使 CPU offline 不能
越过已经选定的 wake target：offline 先关闭 rq admission，再等待在途 publication/rq holder，
最后迁移 queued 实体；不能把旧 inbox quiescent 当作 offline 完成条件。

### 物理门铃

deadline、owner control 与 deferred task-work 仍使用逻辑 sticky work。#1916 引入的
`ax-ipi::DeliveryEdge` 是所有 IPI 用户共享的唯一物理 coalescer；ax-runtime 不再为 scheduler
维护第二套 doorbell generation：

1. producer 先发布 inbox/payload 及 ax-task 的逻辑 request generation；
2. `ax_ipi::notify_cpu()` 把该 publication 映射为一个可合并的物理 edge；
3. 已 armed 的 edge 覆盖并发 publication，不复制逻辑 pending 状态；
4. handler 入口先调用 `ax_ipi::claim_current_delivery()`，再读取各逻辑 owner；
5. drain 期间的新 publication 看到已 claim 的 edge 后可以重新发送 IPI。

`RuntimeStatus::Busy` 不再表示 coalescing：成功意味着当前 generation 已由新边或在途边
覆盖，错误只表示运行时无法兑现投递。全局 deferred task-work 同样以
`published_epoch/claimed_epoch` claim 批次，不再用一个 bool 同时表达“有 work”和“已被
consumer 观察”。

idle polling 与 work pending 共用原子状态，确保“观察到 polling 就省略 IPI”和“退出 polling 后新工作一定有物理边”之间没有丢唤醒窗口。

普通 remote wake 不再进入这个门铃。这里删除的是旧 task-level `RemoteWake` inbox；
migration、affinity reconciliation 等 owner-only 控制仍使用 typed `owner_control_inbox`，
不能把二者混写成“所有 inbox 已删除”。waker 已在 target rq 内完成 activate 后，只有
`wakeup_preempt()` 等价判断确认新实体应抢占目标 current，才发布 target
`need_resched` 并发送 reschedule IPI；目标处于 polling idle 时只发布状态，不发送
物理 IPI。旧 `RemoteWake` inbox、嵌入线程的 wake node 和 drain batch 在切换完成后
直接删除，不保留 feature fallback 或兼容别名。

运行中线程的 notification 采用 Linux `try_to_wake_up()` 的 current-task 快速路径语义：
在线程 scheduler state 锁内发布 sticky wake bit 后立即返回，不选择目标 CPU、不预约
runqueue publication，也不取得 rq lock。该 bit 由随后的 `prepare_park()` 消费，因此既
保留 wake-before-park 唯一胜者语义，也避免普通 channel/completion 的冗余通知为每次 wake
付出一次全 CPU placement 扫描。确定性回归直接统计 target-selection 边界，旧实现为 1，
新实现为 0，并继续断言随后 park 返回 `Notified`。

### Fair/EEVDF 唤醒与迁移

Linux v7.1 的 Fair 唤醒不再用旧的物理时间 `wakeup_granularity` 修正虚拟
deadline。当前实现直接使用 wrap-safe 的虚拟时间顺序，并保留最新 EEVDF 的请求保护：

- wake 后先以包含 current 的加权平均虚拟时间判断 eligibility；
- 普通 Fair 的 normalized request 与 Linux v7.1 一致为 700 us，并按
  `sched_tunable_scaling = SCHED_TUNABLESCALING_LOG` 使用
  `1 + ilog2(min(nr_cpu_ids, 8))` 放大；因此 4 CPU 配置的实际 request 为 2.1 ms。
  初始实体只获得半个实际 request，之后的 request 以及 sleep 后新 request 使用完整
  slice；
- eligible current 的活动请求未结束时继续运行，不因任意更早 deadline 立即切换；
- current 已 ineligible 时保护失效，正 `vlag` 的唤醒线程可在本次 safe point 抢占；
- Fair request 到期由 task deadline/clockevent 保证有界重选，不依赖旧粒度阈值；
- wakee 只有同时击败 current 的 request protection，并且确实是整个 runqueue 的
  earliest eligible EEVDF 候选时，才能发布抢占。只比较 current 与 wakee 会在队列里
  已有更优候选时重复发送 reschedule，Linux v7.1 的 `pick_next_entity()` 不允许这种放大。

`FairRunQueue::zero_vruntime` 是每个 Fair mode 唯一的平均虚拟时间状态。插入和移除
实体允许平均值前后移动；公平性由 dequeue 前保存的 `vlag` 保证，不能再用第二个
单调 `virtual_time` 把平均值强行取大。

跨 CPU placement 与 balance 不能使用线程数量替代负载。Linux v7.1 的
`cfs_rq::avg`/PELT 以 `load.weight` 维护可运行负载，并结合 CPU capacity 计算 imbalance；
因此一个 nice -20 实体与多个 nice +19 实体绝不等价。当前阶段采用不引入第二时间源的
瞬时 demand 模型：Fair 实体直接累计 Linux nice weight，RT/Deadline 实体暂按一个
nice-0 capacity unit 计入总 demand。runqueue 已维护的 Fair total-weight 是唯一数据源，
发布 load summary 不扫描线程；current 与 queued demand 在 rq lock 的同一事务内发布。

初始 placement、空闲 CPU source 选择和周期 Fair balance 统一使用该 demand。周期迁移
只有在候选移动后 source/target 的绝对 imbalance 严格下降时才允许提交，避免把两个
nice +19 轻任务从低负载 CPU 推向已有 nice -20 重任务的 CPU。owner-to-owner carrier
显式携带并预留候选 demand，drain 或 publication 回滚时精确释放，因此并发 placement
不能把尚未物理入队的重任务当成一个普通计数。后续若引入 PELT/CPU capacity，应扩展这一
权威 summary，而不是重建按线程数的兼容旁路。

sleep 与 migration 语义分开：sleep 保存 `vlag`，wake 时开启新请求；runnable
migration 同时保存 `vlag` 和相对 deadline，在目标 runqueue 恢复同一个活动请求。
所有 queued migration 统一执行：

```text
update source V -> capture vlag/relative deadline -> detach
-> begin_queued_migration -> publish carrier
-> destination place / source rollback
```

affinity 和 periodic balance 不再各自维护一条裸 `dequeue` 路径。失败回滚恢复原队列
位置、placement、deadline bandwidth 和 Fair 请求状态。

本地 `next` 提交后的 balance 与本地切换正确性分层。Linux v7.1 的
`finish_lock_switch()` 只在调度尾执行无返回值的 `__balance_callbacks()`；Fair
balance 遇到 pinned/affinity 竞争时只累计失败并通过 active balance 异步重试，不能
否定已经选定的本地任务。对应地，TGOSKits 的 owner-to-owner balance 事务返回
`Migrated / NoCandidate / Retry`：目标 offline、affinity 改写、候选失效或 publication
竞争在完整回滚后归为 `Retry`，不会再触发 scheduler fatal。只有回滚本身无法恢复
placement、队列或 Deadline bandwidth 时，才作为本地不变量破坏返回错误。

## Active mm 与地址空间所有权

Linux v7.1 的 `context_switch()` 不在 user task 切到 kthread 时恢复一个独立 kernel mm。
kthread 通过 `enter_lazy_tlb()` 借用前一任务的 `active_mm`；切回同一 mm 时无需再次写
CR3/SATP 或刷新 TLB。真正的 mm 引用释放在 `finish_task_switch()` 之后，PREEMPT_RT
通过 `mmdrop_sched()` 把可能阻塞的最后释放推迟到安全上下文。

但 Linux 的任务所有权与物理 active-mm carrier 不是同一个生命周期。每个退出线程先在
`do_exit()` 内执行 `exit_mm()`，清除自己的 `task_struct::mm` 并完成 `mmput()`；只有
`active_mm`/lazy-TLB carrier 继续把页表根存储保留到后续 switch tail。`exit_notify()`
发布 zombie 时，退出进程的用户映射已经拆除，不能让父进程 `waitpid()` 返回后再依赖
异步 task record reaper 释放匿名页。

TGOSKits 采用同一套最新语义，不保留旧的“`usize` 页表根 + kernel thread 强制恢复
kernel root”接口：

1. Starry 为每个调度线程创建 move-only `TaskAddressSpace`，其中保存 OS owner；
2. ax-task 只复制借用型 `AddressSpaceHandle` 到 dispatch 元数据，唯一销毁权保存在
   `AddressSpaceToken`；
3. 四架构 `TaskContext` 只保存寄存器、TLS 和 FP 状态，不保存页表根；context 创建请求
   也不携带地址空间，彻底删除第二条隐式安装路径；
4. ax-task 在切换边界提交显式 `KernelLazy` 或 `User(handle)` 激活事务，不再把
   `AddressSpaceHandle::NONE` 作为 runtime 自行猜测的兼容入口；
5. ax-runtime 的每 CPU `ACTIVE_ADDRESS_SPACE` 持有物理 active-mm lease；
6. user -> kthread 保留 lease，kthread -> same user 不重新写根或 flush；
7. CPU offline 的 kernel-root 恢复是独立事务，不复用 kernel-thread lazy 激活路径；
8. 切到另一个地址空间或 CPU offline 时，先提交新硬件根和 per-CPU pointer，再释放旧
   lease；最后一个 lease 若观察到 reclaim waiter，只发布 allocation-free task-work；
9. task-mm owner 提供一次性的 task-detach edge；它可以在普通任务上下文解除 OS
   scheduler slot，但其 owner 对象和地址空间存储仍由 runtime wrapper 保留；
10. runtime wrapper 和页表根存储只在最后 CPU active-mm readiness edge 后由普通任务
    上下文 reaper 销毁；thread-private 资源不受 active-mm lease 拖延。

统一的是 owner/token/lease 生命周期，物理安装按架构实现：

| 架构 | kernel thread 执行时的用户根 | active-mm 语义 |
| --- | --- | --- |
| x86_64 | 保留借用 mm 的 CR3 | 同 root 不写 CR3，不 flush |
| RISC-V | 保留借用 mm 的 SATP | 同 root 不写 SATP，不 flush |
| AArch64 | TTBR1 保持 kernel，TTBR0 使用禁用值 | owner lease 仍保留到下一次切换 |
| LoongArch64 | 保留借用 mm 的 PGDL，PGDH 保持 kernel | 同 root 不写 PGDL，不 flush |

按需缺页补齐 PTE 与“旧映射失效后回收”不是同一种 TLB 事务。前者仿照 Linux
`update_mmu_cache()`，只在 fault 返回用户态前同步当前 CPU 的 fault VA：x86 不缓存
invalid leaf，AArch64 依赖一致的硬件 walker，因此两者保持空操作；RISC-V 执行本地
`SFENCE.VMA`；LoongArch 软件 refill 可能安装 `NR|NX` 占位项，因此本地 invalidation
后重新 refill。这个 fast path 不发送 IPI，也不释放任何物理所有权。只有 unmap、COW
替换或权限收紧等可能让其他 CPU 持有危险旧映射的事务，才进入 active-mm tracker 与
同步 shootdown。

exec 的提交边界固定为：IRQ-off 内先验证 current context 和新 runtime object，再转移
scheduler token；转移之后只有不可失败的 context/root/active-mm 提交。旧 token 的
销毁、OS owner drop 和回收队列扩容只能在恢复 IRQ 后发生。创建失败时 token 仍由
`TaskAddressSpace` 持有并按事务逆序释放，不提供 raw-root 构造或回退入口。

线程退出使用另一条显式事务：

```text
IRQ off: take current AddressSpaceToken -> scheduler mm = NONE -> KernelLazy
IRQ on : detach task owner once -> release token to active-mm reaper
last Linux thread: release process slot -> clear user mappings -> publish zombie
later switch tail: last active CPU lease -> destroy runtime wrapper/root storage
```

`new_with_task_detach()` 的 owner 必须在 detach 后继续保持页表根存储有效，直到其最终
drop；detach callback 只允许释放任务级记账或 OS ownership，不能释放仍被 CPU 装载的
root。Starry 的 `SchedulerAddressSpaceLease` 因此把“scheduler slot 已解除”与“保留
`Arc<AddrSpace>` 存储”建模为同一对象的两个阶段，原子的一次性 release 同时覆盖正常
退出、exec 替换和创建回滚，避免双减计数。

### 用户地址空间 TLB shootdown 与延迟回收

调度 token 与 Linux `mm_struct` 不是同一身份。一个 `CLONE_VM` 进程可以为每个线程
创建独立的 runtime token，但所有 token 仍共享同一个页表根；因此 active CPU footprint
必须属于 Starry `AddrSpace`，不能属于某一个 scheduler token。当前模型为：

1. `AddrSpace` 创建唯一且永久绑定页表根的 `AddressSpaceCpuState`，同一页表根的所有
   runtime token 都保存该 tracker 的强引用；构造器拒绝把 tracker 复用于另一个 root；
2. CPU 切入新 mm 时先以 Release 发布 CPU bit，再安装页表根；切出时先安装新根，再清除
   旧 mm 的 CPU bit；同一 tracker 的线程切换不反复清位；
3. 修改方先完成 PTE mutation，再以 Acquire 快照 active mask。快照前已经发布的 CPU
   必须参加 shootdown；快照后进入的 CPU 会在安装根时执行本地 invalidation，因此看到
   新 PTE；
4. `TlbGather` 只保存经过验证的虚拟地址值区间、待释放页框和 backend owner，不保存
   task、回调或裸指针。范围验证必须发生在 PTE 修改前，修改后 range record 是不可失败
   的内部不变量；
5. 同步 IPI 完成前不递减 COW frame reference，也不释放 file/shared/linear backend。
   shootdown 失败时宁可保留所有权，也不能把旧 TLB 指向的页框交回分配器；
6. 单区间、单页框和单 backend 使用首项内联存储，普通 COW fault 与单页 cache eviction
   不为 gather 分配容器；多个不相邻区间才扩展 overflow storage。

这对应 Linux v7.1 的三条边界：RISC-V `switch_mm()` 在装载 SATP 前更新
`mm_cpumask()`；`flush_tlb_mm_range()` 只向该 mask 发送同步 shootdown；
`mm/mmu_gather.c` 在 TLB invalidation 完成后才释放页表和物理页。范围与全量 invalidation
共享四架构统一前端，但成本策略由架构后端决定：当前 4 KiB 归一化接口采用 x86 默认
33 页、RISC-V 默认 64 页；AArch64/LoongArch 暂沿用页表引擎已有的 32 项有界批次，
不把该经验值描述为 Linux 固定常量。未来若引入 ASID/PCID 或大页 stride，必须把它们
加入架构 cost model，而不是在 Starry 地址空间层复制另一套阈值。

## Task deadline 与 clockevent

时间值分为两个不能直接比较的域：

- scheduler `rq_clock` 使用无符号回绕时间戳，所有相对参数必须小于半区间，先用有符号差
  判断先后；Deadline CBS、RR/Fair request 与运行时间记账保存这个域的值；
- 物理 timer 使用 Linux `ktime_t` 的有限有符号域。`MonotonicInstant` 与
  `MonotonicDeadline` 只在 runtime/clockevent 边界出现，`KTIME_MAX` 仍是有限时间，
  只有 `Option::None` 表示没有期限。

两域只能在 owner rq 已取得一致 scheduler clock 样本时转换：先计算 scheduler future
相对当前 rq clock 的正向 delta，再把 delta 加到同一事务采样的 monotonic instant。
禁止把 scheduler 绝对值和物理绝对值直接 `min`、比较或共同存进 raw `u64` 缓存。
这对应 Linux v7.1 `start_dl_timer()`：`rq_clock()` 空间的 release/deadline 通过 delta
映射到 `ktime_get()`，而不是假定两者绝对纪元相同。

`CpuRunQueueState` 现在像 Linux `struct rq` 一样独占 `RunQueueClock`。它只在目标 rq 的
IRQ-safe lock 内调用 `TaskRuntime::rq_clock_sample(RuntimeCpuId)` 接受一个已校正 clock 和
累计 hardirq 时间样本：首样本建立基线，负向 delta 被拒绝，counter wrap 按 signed-delta
顺序前进。远程
wake 必须锁定目标 rq 并读取目标 CPU source，不能把 waker CPU 的时间带入目标实体。
一次 owner rq 事务只允许更新一次；后续 dispatch settle、switch plan 和 timer programming
读取同一个 `RunQueueClockSnapshot`，对应 Linux 的 `RQCF_UPDATED` 与 `rq_clock()` accessor，
不得通过第二次 source 读取制造微小双重记账。common IRQ entry/exit 是 hardirq 累计时间的
唯一 authority，rq 以 `prev_irq_time` 增量构造 `clock_task`；当前没有独立 steal-time
authority，因此不虚构 steal-time。

`TaskRuntime` 因而分别提供按 CPU 取样的 `rq_clock_sample(cpu)` 与
`monotonic_now()` 两个 capability。平台当前可以让两者读取同一个硬件 counter，但调用方
不能据此合并接口或依赖相同 epoch。
timer IRQ 使用入口的 monotonic 样本提升物理 task deadline，另取 scheduler 样本完成
runtime charge、scheduler tick 与 CBS 状态转换，最后再取 monotonic 样本把下一 scheduler
delta 映射到物理 clockevent。普通 `__schedule` 等价 fast path 在 task deadline heap 为空且
没有 sticky deadline work 时只读取 scheduler clock，不额外触碰物理 timer clock。

Fair 周期 balance 不属于 rq-clock deadline。Linux 的 `sched_balance_domains()` 以全局
`jiffies` 和 `sd->last_balance` 判定周期，并在 pass 完成后更新下一期限；TGOSKits 对应使用
monotonic deadline、`armed -> pending` sticky publication 和 owner task-context consume。
clockevent 负责把到期 cadence 发布为 work，普通 schedule 不扫描时间；newidle balance 则像
Linux 一样立即尝试，不受周期 deadline gate 限制。selection/carrier 不携带时间戳，源与目标
rq 的 mutation 分别在自己的 owner 事务里接受本地 rq clock。

`start_dl_timer()` 对已经过去的 scheduler deadline 返回 false，不建立 hrtimer。当前
实现同样只给 `Future` 建 `TaskDeadlineQueue` 节点；`Due` 由 owner 在持有线程 scheduler
状态和目标 rq 所有权的同一个事务内直接完成 miss 观察、CBS replenish、unthrottle、
enqueue 或 zero-lag bandwidth 释放。不得把 `Due` 改写成 physical-now 节点，也不得等待
下一次偶然 timer IRQ 才推进。确定性回归要求：已过期 CBS refresh 后没有物理 timer
registration，并且调度状态已经在本次 owner 事务中完成转换。

`ax-task::TaskDeadlineQueue` 只接受：

- sleep/park/wait timeout；
- RR/Fair/Deadline 调度期限；
- ax-task 自身 deferred task-work deadline。

条目只保存 `ThreadId`、generation、typed kind 和有限 deadline，不保存闭包、OS 对象或驱动对象。rearm 必须物理替换旧节点，cancel 必须物理移除；不得以 tombstone 占容量。

`ax-runtime::LocalClockEvent` 是以下状态的唯一 owner，每次 CPU online/offline 转换都会
推进不可回绕的 lifecycle epoch：

```text
Offline | Idle | Armed(deadline) | Firing
```

timer IRQ 顺序：

1. platform claim/ACK；
2. 对照当前 epoch 与 arm：`Idle/Offline/Firing` 的 stale edge 不进入 ax-task，但必须先
   stop/mask 物理 clockevent；任何有效 `Armed` edge 都进入 `Firing(token)` 并失效旧 arm，
   early edge 的有界扫描不产生 due work，finish 再统一重编程一次；
3. 非 idle CPU 更新 scheduler tick；idle/nohz 状态不生成 periodic source；
4. 调用有界 `on_clock_event(now, budget)`；
5. 发布 sticky deadline work / need-resched；
6. 合并 task deadline 与当前有效的 scheduler tick，统一编程一次；
7. 返回平台做 EOI。

clockevent stop/mask 与控制器 ACK/EOI 是两个层次。若逻辑 `Ignored` 只返回而不静默物理
level/pending source，EOI 后会立即重入并形成 IRQ storm。RISC-V net-loopback 的 GDB
证据曾观察到超过 140 万个物理 timer edge，却只有 40 次有效 `Firing`；对应最低层回归
固定要求 `Ignored -> ClockEventAction::Stop`。四架构后端分别负责 mask/disable/compare
更新，trap/IRQ 入口继续独立完成控制器 claim/ACK/EOI。

`finish` 必须消费同一 move-only firing token。token 的 epoch 已过期时不能发布
task deadline、periodic advance 或硬件动作。这样 offline 前已经 pending 的 IRQ 即使在
re-online 后才交付，也不能把新 CPU 周期提前推进到 `Firing`。

无期限用 `Option<MonotonicDeadline>`，不能用 `u64::MAX` 直接下发硬件。用户绝对时间按
Linux `timespec64_to_ktime()` 规则饱和到有限 `KTIME_MAX`；相对 timeout 按
`ktime_add_safe()` 饱和。ns 到 tick 使用向上取整和饱和转换；已过期值只在物理设备边界
按 clockevent 的最小非零 delta 编程，不回写逻辑 deadline。

物理 clockevent 是 deadline 推进的唯一正式入口，不是可丢边后由调度器轮询补救的加速路径。
硬中断只把有界数量的 task deadline 从 heap 提升到预分配 expired buffer，并发布每 CPU
`ktimers/<cpu>` worker 的 sticky wake；真正的 sleeper wake 只在该 FIFO worker 的任务上下文
执行。scheduler safe point 不扫描 task deadline heap，也不提供 `claim_due`、
`recover_overdue` 或偶然 tick 恢复。硬 scheduler timer 的预算 remainder 仍由统一 scheduler
request 迫使 owner safe point 继续处理，两者不能互相代替。

旧测试 runtime 曾让 `*_at(now)` 同时隐式代表 scheduler 与 monotonic 时间，并在多个新建
`TaskSystem` 场景间保留同一 fake source；这既掩盖跨 CPU/rq epoch 错误，也让物理 timer 在
测试中无条件提前到期。当前 fake clock 按 CPU 分开，测试必须在每个 CPU 生命周期建立前
初始化 scheduler source，并显式推进 monotonic clockevent。确定性回归覆盖：目标 CPU 与
waker CPU 使用不同 rq epoch、未来 monotonic deadline 不被较大的 scheduler 时间提前触发、
Fair cadence 相对 CPU online 的 monotonic 时刻建立、zero-lag/CBS 两阶段事件只在物理
clockevent 后推进。测试不得用 no-op timer、共享全局时间或源码字符串断言替代这些行为。

Linux 在 `dequeue_task_dl()`、`inactive_task_timer()` 与 switch-tail 的
`task_dead` 回调中，都由持有对应 rq 所有权的一侧移除 Deadline bandwidth；任务退出侧
不能越过 rq owner 直接修改计账。TGOSKits 同样把阻塞线程的 reservation 清理投递给
原 owner CPU。退出事务若为阻止新调度访问而暂时关闭 scheduler activity gate，必须先
放弃该排他 permit、重新开放普通 owner-control publication，再发布清理请求；不能让
退出路径通过普通入口向自己已经关闭的 gate 投递消息。registry 回收继续等待 bandwidth
和在途 delivery 同时归零。

## PI、等待与锁边界

### 唯一 sync 分层约束

所有生产锁算法、状态机、PI waiter tree、lockdep 与 guard context transaction 只允许由
`ax-task::sync` 实现，以本分支实现为唯一事实源。`ax-sync` 只保留 OS 无关 wrapper、稳定布局和
外部 capability ABI，不得依赖 `ax-task`、`ax-hal`、`ax-runtime` 等 OS 组件，也不得按
`host-test`、`target_os` 或 feature 提供第二套锁算法。接口若妨碍该分层，可以破坏性调整，不保留
旧 trait、旧 provider 或转发兼容层。

`ax-runtime::sync` 是 OS capability 的唯一 provider，并重导出 `ax-task::sync` 的锁 API；
StarryOS、`ax-std`、Axvisor 与其他 OS consumer 只能经过 runtime facade 使用这些锁，不能直接依赖
`ax-sync`。因此 `ax-sync` 的职责是让 OS 无关组件通过外部函数接入唯一实现，而不是成为另一套可独立
选择的 lock engine。PR #1962 只提供这种分层理念，具体锁语义与实现继续以本分支为准。

### PI mutex

ax-sync 与 ax-task 的 PI registration、release 和 claim 遵循 Linux rtmutex 的事务边界：

1. `PiMutexCore` 唯一拥有物理 owner word、mutex generation 和按 urgency 排序的 waiter
   tree；硬中断不访问该状态，task fast path 只在 CPU pin 下操作 owner word；
2. waiter tree 的 raw ticket gate 对应 Linux `rtmutex->wait_lock`。固定锁序为
   `TaskSystemState -> PiMutexCore.waiters`；ax-sync 不再拥有一个额外的 slow-path gate，
   ax-task 也不会在持有 per-lock gate 时反向取得调度图锁；
3. registration 在一个事务中验证 owner snapshot 和 donation graph，以 waiters bit 排除
   fast unlock，再把线程内嵌的 lock waiter linkage 插入 per-lock tree，并把该锁 cached top
   挂入 owner donor tree。失败发生在任何 publication 前，不再向 ax-sync 暴露 prepared
   transaction 或半提交状态；
4. release 在同一事务中移除旧 owner 的 top donation、选择 waiter、完成 deboost，再以
   Release 发布 ownerless owner word。selection 必须先于 ownerless word 可见；新 contender
   因 waiters bit 不能窃取已经选中的 handoff；
5. claim 只接受原 registration 生成的 move-only `PiWaitToken`，验证 mutex generation、
   selected generation 与 current thread 后，删除线程 waiter linkage、发布新 owner/grant，
   再把剩余 cached top 挂入新 owner donor tree；
6. ax-sync 只保存 `PiMutexCore` 和 waiter sequence，不保存第二份 owner、selected、本地
   waiter 链或 pinned waiter 生命周期。block 与定向 wake 都在调度图和 per-lock gate 释放后
   执行。

这个顺序对应 Linux `rt_mutex` 的 `wait_lock -> task->pi_lock` 分层和 `wake_q` 锁外唤醒语义。
owner word 是物理持有者的唯一权威，ax-task per-lock tree 是调度顺序与 selected token 的
唯一权威。旧的跨 gate prepared transaction 会短暂发布 ownerless word、却尚未发布
selected token；claim 或新 waiter 恰好跨过该窗口时会把旧快照带进下一次 scheduler
事务，最终得到 `ownerless handoff has no selected scheduler waiter`。QEMU/GDB 已在
`test-cargo-jobserver-wait` 的 pthread/pipe 波次确定性定位到这一状态，因此不采用局部重试，
而是删除 ax-sync 的第二个 gate，把 owner word、waiter tree、donation graph 和 selection
纳入 ax-task 的单一事务。per-lock gate 是不关中断的 raw ticket gate，硬中断不访问 PI
metadata；实际 block/wake 始终在 gate 外完成。

Linux v7.1 的 `rt_mutex_adjust_prio_chain()` 默认允许较深的链并在遍历中提供可抢占点；当前
ax-task 的 donation graph 仍由一个不可抢占事务保护，因此不能照搬 Linux 的 1024 层默认
值。`TaskSystemConfig::pi_chain_limit` 默认限制为 64，所有 fallible chain validation 都在
任何 mutation 前完成；超限返回 `PiChainLimit`，旧 donation 保持不变。这个上限约束的是
锁嵌套深度，不是同一锁的 waiter 数量，不能用拒绝第 65 个 waiter 来掩盖 per-lock 扫描。
当前实现已经采用与 Linux `rt_mutex_waiter`/`pi_waiters` 相同的双层所有权：每把锁的 AVL
tree 保存全部 waiter，owner 的 donor tree 只保存每把锁的 cached top。waiter urgency
变化只在所属 lock tree 中删除并重插一个预备节点；top 变化时只替换 owner tree 的一个
donor。registration、policy update、release 和 claim 均不再按 owner 或 registry 扫描全部
waiter，ax-sync 的本地链也不参与调度排序。

`pi_mutex_claim()` 只接受原 registration 的 `PiWaitToken`，不再让上层重复传入 claimant 或
lock identity。facade 同时验证 current thread 与 token identity，避免把另一线程的 waiter
publication 提交到 scheduler。claim preflight 把当前 selected edge 按“即将 detach”建模；
不能在已持有该 lock waiter-tree guard 时沿旧 `blocked_on` 再次进入同一 raw gate。该边界
对应 Linux 在 owner handoff/fixup 中先确定 waiter/owner 状态，再做 prio-chain 调整，而不是
递归获取同一 `wait_lock`。

`PiMutexId` 与 waiter registration 均带 generation，锁销毁前必须 quiesce，防止地址复用
ABA。任务等待通过 park/completion 睡眠，不在禁抢占区做无界 spin。ax-sync host runtime
的 CPU、guard、TaskSystem 指针和 IPI 观测也必须是测试线程局部状态；进程全局 fake 会让
并行测试彼此清空 guard depth 或借用过期指针，既制造假失败，也会掩盖真实事务顺序。

可中断获取必须直接复用同一 PI registration，而不能在外层用 `try_lock + yield` 轮询。
Linux v7.1 `rt_mutex_slowlock_block()` 每轮先执行 `try_to_take_rt_mutex()`，只有仍未取得锁时
才检查 pending signal；若退出等待，则在 `wait_lock` 下移除 waiter 并同步撤销 owner
donation。因此 ax-task 的取消事务显式返回 `Cancelled | HandoffPending`：前者证明 waiter
和 donation 已一起撤销，后者证明 unlock 已把 ownerless handoff 发布给该 waiter，调用方
必须先 claim。`pi_park_current_once()` 只执行一次 park，任何 signal/exit wake 都返回
ax-sync 的 rtmutex 状态循环重新检查，而不是在 facade 内部吞掉无关 wake。Starry exec 的
`cred_guard_mutex` 等价锁使用这个接口并只把 sibling `exit_request` 视为 kill 条件，删除
无界 `yield_now()` 忙等。

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

非 leader exec 的 TID 转移对应 Linux `de_thread()` 在 `tasklist_lock` 下执行
`exchange_tids()`：Starry 在同一个 `TASK_TABLE` PI guard 内验证 scheduler generation、
预先插入目标 key、更新 `Thread::tid` 并移除旧 key，因此 task lookup 不会观察到 key 与
线程身份不一致。旧线程 TID 的 namespace reservation 只在 signal child 与 thread-group
索引完成重命名后释放；进程 PID 仍由既有 `ProcessIdentity` 持有，不新建第二套 PID owner。

`waitpid` 与 `waitid` 对应 Linux `__do_wait()` 的循环语义：仅 syscall 入口的第一次扫描可
决定“当前没有符合条件的 child”并返回 `ECHILD`；一旦进入阻塞，每次 event wake 和
check-versus-register 二次检查都必须从 parent children、live ptrace registry 与 traced
zombie registry 重新生成候选。不得让 `Vec<Arc<Process>>` 跨 park 保存，因为等待期间
可能出现新 child、reparent、ptrace attach/detach 或新的 zombie publication。P_PIDFD
继续用 `ProcessIdentity` 的 generation 精确匹配，不能退化成裸 PID。

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

#1852 的控制器/action 启用次序、xHCI Linux 对照、失败回滚与新调度器 Waker
适配见 [USB IRQ lifecycle for the task-based scheduler](usb-irq-lifecycle.md)。该问题的
所有权在 USB controller 与 framework action 生命周期，不修改 `ax-task` 正确性边界。

xHCI host 初始化必须取得真实 IRQ binding；缺少 IRQ 时 probe 直接失败。旧的可选 IRQ、
永久 1 ms USBFS event ticker 和 PCI interrupt-disable fallback 已删除，不保留 feature
开关或兼容入口。

Starry evdev 的后台 `evdev-poll` 兼容线程同样删除。输入 wake 只允许沿设备 IRQ、
`IrqWaitCell` 和任务态 IRQ service worker 推进；IRQ 路由失效必须在平台/驱动所有权
边界修复，不能在上层用 10/200 ms 轮询掩盖。

vsock hard/poll 路径只发布固定事件与 credit snapshot，connection manager 和 socket wake 在释放设备 gate 后由 worker 处理。第三方 manager 吞掉 `CREDIT_REQUEST` 细节的问题由 issue #1724 跟踪。

## 主要确定性红绿证据

| 缺陷 | 修复前确定性表现 | 修复后不变量 |
| --- | --- | --- |
| AArch64 guard 所有权 | task B 继承 task A 的 depth；QEMU panic `unbalanced CPU-local preemption guard exit` | load/store 架构 depth 跟随 `CurrentThreadHeader` |
| current 指针校验 | 错位 publication 返回 `Ok(0x1)` | guard 访问前返回 `CurrentThreadMismatch` |
| remote affinity | completion 在目标 owner 真正 enqueue 前完成 | generation completion 在 destination commit 后发布 |
| clockevent 丢边 | overdue sleeper 永久挂起 | scheduler safe point 有界恢复 overdue deadline |
| PI 地址复用 | 新锁可匹配旧 donation edge | `PiMutexCore` generation 永不复用 |
| PI 锁对象膨胀 | waiter linkage、owner 和 handoff 在 ax-sync/ax-task 重复保存，`RawMutex` 为 152 B | linkage 全部归线程、owner/tree 归 `PiMutexCore`，`RawMutex` 收敛为 64 B |
| PI owner 扫描 | blocked waiter urgency 变化时 prepare/apply 两次扫描 owner 的全部 waiter，两个 waiter 确定访问 4 次 | per-lock AVL 重排一个节点，owner donor tree 只替换 cached top，访问计数为 0 |
| PI claim 自锁 | 持有 lock waiter-tree guard 后沿 claimant 的旧 `blocked_on` 重进同一 raw gate，ownerless claim 永久停滞 | token-bound claim preflight 把 selected edge 视为待 detach，证明链深度为 1，commit 后再从新 owner 的 remaining top 重算 |
| IRQ waiter | 第二次 IRQ 可被注册尾清掉 | 单原子状态线性化 Pending/Waiter/Notifying |
| IRQ registration ABA | 旧 detach 在 generation 检查后暂停，IRQ 完成并以同地址发布新 generation；恢复后旧 CAS 删除新 waiter 并 panic | IRQ 完成进入 `Draining`；旧 token 完成 grace 前同地址节点不可 rearm |
| signal ack | scan 后并发 SIGKILL 被 boolean clear 擦除 | generation ack 不越过新 publication |
| perf migration | 旧 CPU slot 留下 stale wake pointer | owner-CPU teardown + registry generation + grace |
| CPU timer | reader等待已被抢占 writer，系统 livelock | owner-only vtime writer + 原子 group aggregate |
| clone publication | PID/TID 可见后 placement 失败再回滚 | stage scheduler first，identity commit 后只做 infallible activate |
| futex wake | syscall 每次 wake 后额外 yield | wake publication 自己驱动 reschedule |
| futex waiter | 每次排队单独分配 `Arc<WaiterState>`，取消与 requeue 依赖对象地址 | 稳定 `Thread` 内嵌 generation 状态；队列用 `UserTaskRef + generation` 校验唯一胜者 |
| alarm worker | `event-listener` 的 no-std 内部自旋锁持有者被抢占后，唯一 alarm worker 永久自旋，进程退出与父进程 wait 停止推进 | 类型化 alarm heap 与 `epoch + WaitQueue` 分离；生产者先发布 generation，释放 alarm 锁后再唤醒固定 worker |
| Deadline scan | 每次 schedule 扫描无关 reservation | typed timer node 和 owner heap |
| same-CPU hard IRQ wake | 测试 runtime 恒返回“不支持本地调度发布”，错误强制 self-IPI | sticky local scheduler publication 先于 self-IPI 抑制，由 IRQ return consume |
| 虚拟多 CPU runtime | integration fake 只清 IPI 布尔值，claim 后目标 CPU 没有 scheduler work，idle/WFI 与 switch tail 是 no-op | 固定容量、零分配状态机执行 publish → physical edge → claim → local work、idle final recheck、context switch 与 tail；CPU offline 必须等待物理 edge 和本地 work quiesce |
| task switch 暂态 | thread placement 同时保存 `SwitchingOut/ExitedAwaitingTail`，与 per-CPU `SwitchHandoff` 双写 outgoing stack 生命周期 | thread 立即提交最终 `Queued/Migrating/Detached`，独立 `on_cpu` 只由 CPU handoff tail 清除；退出暂态不再写入 task placement |
| Fair sleep wake | wake 清空正 `vlag`，维护线程 Ready 后仍等到偶然 timer | dequeue 保存有界 `vlag`，ineligible current 在 IRQ-return safe point 立即让出 |
| Fair virtual-time wrap | `saturating_add` 与普通 `<` 在 wrap 后颠倒 deadline | 所有虚拟 deadline 使用 modular `virtual_before` |
| Fair current 过度抢占 | 删除旧 wakeup granularity 后，任意更早 deadline 都打断 eligible current | 最新 EEVDF 请求保护保留到 request boundary；ineligible current 不受保护 |
| Fair 初始放置 | 新线程直接获得完整 1 ms request，与 Linux v7.1 `PLACE_DEADLINE_INITIAL` 不同 | normalized slice 改为 700 us 并按 CPU 数对数放大；初始 deadline 与 oneshot 只给半个实际 request，后续 request 恢复完整 slice |
| queued affinity migration | 从源队列移除后才保存 `vlag`，确定性得到 200 而正确值为 100 | 所有 queued migration 在 detach 前保存源 V，并共用 publication/rollback 事务 |
| Fair 平均虚拟时间 | runqueue 同时维护加权平均与只增不减的第二 V，membership 变化后参考系分裂 | `FairRunQueue::zero_vruntime` 成为唯一 V，32 个固定种子、每种 10,000 事件参考模型一致 |
| switch 后 balance 竞争 | LoongArch `task-parallel` 偶发在已提交 `next` 后把一次 balance 事务竞争升级为 `0x53430001` fatal | balance 事务完整回滚后返回 `Retry`；本地切换继续，只有回滚失败才是 fatal |
| 阻塞 Deadline 线程退出 | exit permit 先关闭 scheduler activity gate，再经普通入口发布 owner-rq cleanup；请求被自身 gate 必然拒绝，reservation 永久保留并持续返回 `ThreadBusy` | 记录 cleanup 后先释放排他 exit permit，再发布 owner-control；确定性测试要求一次 owner drain 物理移除 bandwidth 后才允许最终退出 |
| bootstrap 前置校验失败 | CPU 已有 current 时在进入 `create_thread()` 资源事务前返回，传入的 context/stack/TLS/address-space 失去唯一销毁权 | `UnpublishedThreadGuard` 从 bootstrap/idle 入口第一行接管完整 `ThreadSpec`，所有前置失败与创建失败共用逆序释放路径 |
| user/kthread 地址空间切换 | user -> blk worker -> same user 每次恢复 kernel root，再次写 CR3/SATP 并全量 flush | kthread 借用 per-CPU active mm；move-only token 和 active lease 将回收推迟到 switch-tail 后任务上下文 |
| zombie 前地址空间回收 | 12 轮 SIGKILL/waitpid 后 scheduler token 仍绑在异步 reap 的 thread record，RISC-V `MemFree` 少约 304 MiB，完整组随后小对象 OOM | 每线程按 `exit_mm()` 顺序同步 detach task-mm；process slot 清理后再发布 zombie；RISC-V/x86_64 仅剩约 6/7 MiB 合法开销 |
| active-mm 过期 readiness edge | `destroy_address_space()` 仍报告 `Active` 时也把回收计为成功；同一 token 在一个 pass 内重试 64 次并永久 yield | `AddressSpaceDestroyOutcome::Active` 只重新 arm waiter，不算 progress；下一次尝试必须由新的最后-CPU lease edge 驱动 |
| pthread 栈 VA 跨 CPU 复用 | RISC-V parent 已写入新页，remote child 仍用旧 TLB 把同一 VA 翻译到已回收页框并跳转到随机地址 | mm 共享 CPU tracker；PTE mutation -> targeted shootdown -> frame/backend reclaim；同一 `test-cargo-jobserver-wait` 13/13 阶段通过 |
| coroutine 最终引用 | 普通任务上下文也把每个零引用 header 投递给单一全局 reaper | 普通任务上下文按最终引用直接释放；只有 hard IRQ 发布类型化 coroutine header，删除任意 callback node、公开 drain 和 shutdown leak fallback |

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
- 调度实体状态与 target runqueue 使用统一 IRQ-safe raw ticket lock，锁序固定为
  thread state -> target rq，IRQ 状态由外层 guard 精确恢复；
- direct wake 在同一个 target-rq 临界区提交 queue membership、current/preemption
  比较和 remotely visible load summary，balance 与初始 placement 不会漏看刚唤醒线程；
- same-CPU hard IRQ wake 发布架构 `need_resched` 后直接由 IRQ return 消费，不发送 self-IPI；
- Fair sleep/wake 与 migration 保留 Linux EEVDF `vlag`，避免维护线程 Ready 后仍排在错误位置；
- eligible current 使用 request protection，避免删除旧 granularity 后形成 wake 驱动的上下文切换风暴。

资源回收进一步采用 Linux v7.1 的最终所有权规则：能在当前任务上下文完成的最后引用
直接释放，不能在 hard IRQ 运行的析构才进入有界 task-work。ax-task 不再提供通用
`DeferredReclaimNode` callback，也不允许 executor 主动 drain 全局 reaper。hard IRQ 只
发布 `CoroutineHeader` 的内嵌类型化节点；consumer 根据固定类型回收，不解释驱动或
OS callback。active-mm 的 `AddressSpaceDestroyOutcome::Active` 表示“没有完成”，不能
消耗 worker progress budget。runtime 边界只接受 `Released / Active` 和 `Ready / Armed`
两组封闭结果；旧的通用 `RuntimeStatus`、`Unsupported`、`InvalidHandle` 兼容返回不再存在，
无效 owning handle 直接作为 provider 不变量失败。

性能接受必须用同一 workload、同一 QEMU 参数、正式 success marker 对比 Linux RT 或已确认基线；发现慢于基线即可中止全量并缩小到目标 case，用 qperf/GDB 检查 wake、lock、IPI 和 safe-point 调用链。

2026-08-03 的同机 x86_64 `qemu/system` 对照先用 `origin/dev` 完整组确定优先级：
dev 为 398/398、guest 汇总 391 秒。本分支在未修复 alarm transport 时两次出现
runner 已打印子测例 BEGIN 或测试主体 PASS、但父进程永久等不到退出的停顿。QEMU
GDB 显示 CPU0--2 均在 idle，CPU3 在
`alarm_task -> timeout_at_wall -> event_listener::Event::listen` 的 no-std 内部自旋锁
循环，且 IF 仍开启。该库在没有 `std` 和 `critical-section` feature 时明确使用不带
内核抢占/IRQ 语义的 `AtomicBool` 自旋锁，并在锁内调用 waker，不能作为可抢占内核
的等待原语。

alarm worker 现已改为同步固定 worker：`ALARM_LIST` 只保存类型化 alarm 值，
`ALARM_EPOCH` 发布变更，`ALARM_WAIT` 负责任务态 park。worker 必须先读取 epoch，
再快照 alarm heap；生产者先修改 heap、再递增 epoch，最后在 heap 锁外 notify。
确定性测试覆盖“publish 正好发生在 worker 快照期间”时 wait predicate 必须立即为真。
修复后的完整轮次连续通过此前两个停点，并完成 398 个与 dev 同名的子测例；最后在
USB audio 驱动路径停止，已作为范围外问题 #1838 单独跟踪。

这轮单次逐项差值只用于选择下一分析对象，不作为 Linux RT 性能结论：

- `test-ext4-inode-unique`：110 秒，对 dev 57 秒，增加 53 秒；
- `test-pagecache-cap`：80 秒，对 dev 51 秒，增加 29 秒；
- `test-x86-avx-ctxsw`：20 秒，对 dev 2 秒，增加 18 秒；
- `test-futex-wake-op-smp`：16 秒，对 dev 4 秒，增加 12 秒；
- `bug-dir-cookie-unlink-rmdir`：21 秒，对 dev 10 秒，增加 11 秒。

因此下一阶段先从最大正回退 `test-ext4-inode-unique` 开始，但必须用 Linux RT 同源
程序区分 ext4/page-cache 自身成本与通用 schedule/wake 放大；qperf 与架构接受指标
只和 Linux v7.1 PREEMPT_RT 对照，不把 dev 当优化目标。

该目标在 Fair sleep `vlag` 首次修复后，按同一 runner 进度投影由约 110--120 秒降至
约 70 秒，说明主要问题是“线程已经 Ready，但 EEVDF placement 使它等到后续 timer”，
不是永久 lost-wake。这个运行按“慢于 dev 57 秒即停止”的规则提前终止，不能作为
最终通过数据；完成单一 V、迁移事务和 request protection 后必须重新测量。

随后核对 Linux v7.1 PREEMPT_RT 的远程 wake 链路发现一处架构差异：
`CONFIG_PREEMPT_RT` 将 `TTWU_QUEUE` 设为 false，`try_to_wake_up()` 在持有 task
`pi_lock`、等待旧 `on_cpu` release 后，直接取得目标 raw rq lock 并激活线程；只有
`wakeup_preempt()` 确认目标 CPU 需要重调度时才发送 reschedule IPI。Linux 的
wake-list payload/IPI 合并路径只用于非 RT 配置。重构前实现对每个远程 wake 先进入
owner inbox，再由 scheduler IPI/safe point 激活，语义上没有永久丢唤醒，但增加了
一次跨 CPU 投递、一次 drain 和不必要的 IPI。当前实现已经用虚拟多 CPU 行为测试
约束 `on_cpu`、CPU offline 和 affinity 并发边界，并以直接目标-rq 事务替换该热路径；
没有保留 wake-list feature fallback、旧 API 或兼容别名。

落实 700 us 基础 slice 与初始半 request 后再次运行同一目标：guest workload 约
45 秒只完成 786/2048，按当前进度仍约需 118 秒；总墙钟 92.98 秒包含约 23 秒
重编译及启动，已按规则主动终止。这个结果证明 Linux 最新初始放置语义是必要的
正确性修复，但不是 ext4 回退的主因；后续不再通过缩短 slice 继续试参，而是直接
验证远程 wake 激活、IPI 数量和 owner safe-point 放大。

### Linux v7.1 PREEMPT_RT 同源唤醒分布对照

旧的 `futex-ping-pong-bench` 只用七轮完整往返推算单向 handoff，无法区分真正进入
内核 park 的样本、同 CPU 与跨 CPU 调度、进程边界和 timer deadline。它已被
`apps/starry/wakeup-latency-bench` 破坏性替换，不保留旧应用或输出兼容。新基准在
同一份 C 源码中逐样本记录从 producer 发布 wake 到 waiter 恢复运行的延迟，并报告
`min / mean / stddev / p50 / p95 / p99 / p99.9 / max` 与固定直方图；`FUTEX_WAIT`
返回 `EAGAIN` 的未 park 样本只进入 `not_parked`，不能伪装成低延迟。

三方固定使用同一宿主、`q35,accel=tcg`、`-cpu max`、2 个 vCPU 和 512 MiB。Linux
内核为 v7.1 提交 `8cd9520d35a6`，配置确认 `CONFIG_PREEMPT_RT=y`。下表是改造
task sleep 前同一次 A/B 的 p50；单位均为 ns：

| 策略与场景 | Linux RT | `origin/dev` | 本分支 |
| --- | ---: | ---: | ---: |
| OTHER，同 CPU 线程 | 23,701 | 80,863 | 176,987 |
| OTHER，跨 CPU 线程 | 26,541 | 158,639 | 149,722 |
| OTHER，跨 CPU 进程 | 27,281 | 163,235 | 152,809 |
| OTHER，绝对 timer | 47,116 | 150,569 | 270,064 |
| FIFO:80，同 CPU 线程 | 15,362 | 75,816 | 178,147 |
| FIFO:80，跨 CPU 线程 | 19,854 | 154,787 | 148,359 |
| FIFO:80，跨 CPU 进程 | 22,565 | 172,705 | 154,635 |
| FIFO:80，绝对 timer | 30,651 | 153,210 | 280,559 |

这个分解排除了“通用 lost wake”：本分支的跨 CPU 线程和进程 wake 比 dev 快约
4%--18%，回退集中在同 CPU park/switch 和 task timer。调用链审计随后确认 Starry
把 `clock_nanosleep` 和 poll 类用户等待的期限放入通用 `TimerFuture`，物理 timer
先唤醒普通 `starry-timer` worker，再由 worker 唤醒用户任务；这比 Linux
`hrtimer_nanosleep -> hrtimer_wakeup -> try_to_wake_up` 多一个 runnable 周期和一次
上下文切换。

修复后，用户任务等待把 absolute deadline 直接传给 ax-task 的 move-only park
transaction；timerfd、设备和任意 future timer 仍由任务态 worker 隔离，hard IRQ
不执行 OS callback。相同基准中 OTHER timer p50 从 270,064 降到 185,580，FIFO
timer 从 280,559 降到 168,875；OTHER 同 CPU handoff 从 176,987 降到 133,955。
这证明移除 worker hop 有效，但仍未达到 Linux RT，剩余固定成本继续从本地 rq
事务、switch tail 和 clockevent IRQ 返回路径审计，不能恢复旧的 wake 后强制 yield。

新时序同时稳定暴露 shell 父子并发 `setpgid()` 的进程组 identity 竞态：两个调用都
通过旧的 check-then-create，并在同一 Session 发布相同 PGID 时 panic。确定性底层
测试先复现第二个 live group 触发 panic；修复后 Session registry 与 Linux 的
`tasklist_lock` 一样成为唯一 identity authority，竞争创建返回同一个
`Arc<ProcessGroup>`，不再发布第二套生命周期。

以上结果只比较相同 TCG 拓扑中的完整 guest 调用路径，不解释为真实硬件的实时上界。
基准协议和每次运行的完整分布是当前验收依据；旧七轮 aggregate 数据只保留为历史
优化记录，不再用于当前性能结论。

首个确定性红测证明，旧实现即使发现 futex 值已经不匹配，仍会分配
`WaiterState`。当前实现已经让调度线程通过 move-only park transaction 直接
进入 `ax-task`，不再为每次 `FUTEX_WAIT` 创建临时 `LocalExecutor`、`WaitQueue`
和 coroutine；同一红测由失败转为通过。

第二个确定性红测进一步要求“已经决定排队的 waiter 也不能分配独立状态对象”。
旧实现稳定报告一次 `Arc<WaiterState>` 分配，x86_64 axtest 为 393/394；重构后
wait 状态固定内嵌在稳定的 Starry `Thread` 中，队列只保留持有生命周期的
`UserTaskRef` 和 wait generation，同一套状态机线性化 wake、signal、timeout、
cancel 与 requeue，axtest 恢复为 394/394。generation 不匹配的旧队列项只能被
清理，不能命中新一轮 wait，避免线程和 slot 复用形成 ABA。

第三个确定性红测来自 x86_64 Starry system 大组中的 `F_SETLKW`：冲突等待线程在
发布领域 waiter 前消费了 sticky scheduler notification，旧实现把这个正常调度提示
混入 `FutexAccessError::Retry`，随后普通 `WaitQueue` 把它当作“不可能的 nofault
访问重试”触发 panic。Linux v7.1 的 `do_lock_file_wait()` 只把 wake 当作重新执行
`vfs_lock_file()` 的提示；信号或退出路径在返回前通过 `locks_delete_block()` 删除
blocker 关系。Starry 因此新增独立的 `FutexWaitError::SchedulerNotification`：普通
等待返回领域层并先析构 POSIX wait-for graph guard，futex nofault 路径清理嵌入 waiter
后直接重试；用户内存 `UserFault/Retry` 仍只属于访问协议。不能在 `WaitQueue` 内部
直接重跑任意条件闭包，否则 fcntl 会在旧 guard 尚未析构时重复注册 wait-for 边。
最低层 axtest 在旧分类上稳定失败，修复后 x86_64 内核 axtest 为 395/395，定向
`bug-fcntl-setlkw-blocks` 的 POSIX、OFD、EINTR 三阶段全部通过。

Axvisor 的真实 VMX 板卡进一步暴露了 host IRQ 入口状态不一致：硬件 trap 天然在
本地 IRQ 关闭时进入 `ax_hal::irq::handle_irq()`，但 x86 VM-exit 会先恢复宿主
RFLAGS，再延迟分发截获的 external-interrupt vector。当恢复后的 IF 为 1 时，IPI
handler 在没有 IRQ publication guard 的状态下发布 scheduler work，正确触发了
ax-runtime 的边界断言。Linux v7.1 的 `vmx_vcpu_enter_exit()` 则要求
`guest_state_enter_irqoff()` 到 `guest_state_exit_irqoff()` 全程保持 IRQ 关闭；退出 guest
状态后，调度工作也只在 `vcpu_run()` 的明确 task-context 边界或
`irqentry_exit_cond_resched()` 中消费。

本分支把该约束放回四架构共用的 ArceOS IRQ 入口：入口首先保存并关闭本地 IRQ，
随后准备平台 IRQ 上下文并进入 preemption guard；处理完成后先在 IRQ 仍关闭时释放
preemption guard，使待调度状态只能通过 IRQ-return baton 被消费，最后才恢复调用者
原 IRQ 状态。真实 trap 的保存状态本来就是关闭，VM-exit 的延迟分发则由同一边界规范
化，Axvisor 不再保留架构专用的补丁或旧兼容入口。

### 2026-08-04 AArch64 vCPU 交接与 console endpoint

Linux v7.1 arm64 KVM 把 guest/VGIC 所有权和可阻塞任务上下文分成两个阶段：
`kvm_arch_vcpu_put()` 先保存 timer、释放 VGIC CPU interface，`kvm_vcpu_block()` 才允许
宿主线程进入 `schedule()`；唤醒后由 `kvm_arch_vcpu_load()` 按 timer、VGIC 顺序重新取得
本 CPU 的 guest 状态。`kvm_psci_vcpu_on()` 也只在目标状态与 reset state 完整发布后执行
`wake_up()`，不会在仍持有当前 vCPU 的 CPU-local guest ownership 时等待目标启动。

AxVM 原 CPU_ON 路径在 `AxVCpu::with_current_cpu_set()` 保持宿主 CPU pin 和 guest binding
期间创建目标线程并等待 startup ACK。目标若需要同一个调度边界，当前 vCPU 无法 switch
tail，最终由 ax-task 正确拒绝在 `preempt_depth=1` 下阻塞。新路径把 HVC 结果类型化为
`DeferredHyperCall::PsciCpuOn`：先退出 guest、保存状态并执行 `vcpu.unbind()`，再启动目标
和等待 ACK，最后把 PSCI 返回值写回。这个 deferred token 是一次性所有权，不保留旧的
“在 HVC handler 内直接阻塞”兼容入口。

GIC maintenance/physical IRQ 已经由硬件入口完成 claim，不能再次进入普通
`handle_irq(vector)` 重复 ACK。四架构共用入口因此区分 raw vector 和 move-only
`IrqId`：`handle_acknowledged_irq()` 只复用 IRQ entry、preemption guard 与 IRQ-return
baton，控制器 claim/ACK/EOI 仍由原 owner 完成。调度工作只能在 guest ownership 释放后
或统一 IRQ tail 消费。

Axvisor console 同样拆成三类独立生命周期：任务态 `ConsoleState` 管理 attach/running 与
backend generation；vCPU callback 只向固定容量 `GuestOutputFrame` 队列执行一次有界
`try_lock` 发布，竞争、队列满或单次超出发布预算时允许丢弃观测性输出；唯一 shell
consumer 在任务上下文完成 generation 校验、行格式化和物理 UART 写入。vCPU 路径不再
取得 `std::sync::Mutex`、分配 `Vec` 或调用可能等待 UART 的宿主接口。旧 endpoint 的
注销只 Release 发布 inactive，已入队帧由 generation 校验丢弃；Arc 负责物理生命周期，
因此不再在 control mutex 内等待 reader 计数归零。

最低层调度测试还固定了 Linux `finish_task_switch()` 对应边界：policy/affinity owner work
在 `switch_handoff` 存在时必须保持 pending，只有 `complete_context_switch()` 清除前一线程
的物理 `on_cpu` 后才能消费。旧测试绕过 production facade 的 switch tail 并期待提前
drain，属于错误测试模型；新断言先验证 tail 前 `drained=0, pending=true`，再验证 tail 后
唯一消费，不能以放宽 owner-control guard 的方式让测试通过。

修复前 AArch64 GICv2 timer-stress 在约 4.6--5.1 秒稳定触发
`UnsafeContext(preempt_depth=1)`；当前 GICv2 正式标志
`AXVISOR_GICV2_TIMER_STRESS_PASSED`，QEMU 34.21 秒、完整命令 44.22 秒。GICv3 同样通过
`AXVISOR_GICV3_TIMER_STRESS_PASSED`，QEMU 33.31 秒、完整命令 43.35 秒。相对同机上一
检查点的 35.26/35.83 秒 QEMU 时间分别下降约 3.0%/7.0%。ax-task 当前为
285 个单元测试、21 个 loom case、11 个 doctest 全通过，AxVM host 单元测试 218/218。
这些时间只证明死锁消除和端到端前进性，不是 Linux RT 同 workload 延迟对比，不能据此
宣称已经达到 Linux RT 性能水平。

同配置完整 guest benchmark 的中位 handoff 从 138,786 ns 降到 111,948 ns，
改善约 19.3%；这证明分配与共享引用流量确实位于热路径，但仍为 Linux RT 的约
8.05 倍，不能据此结束性能审计。

wake 侧的临时 `Vec<ThreadWakeHandle>` 已由 ax-task 的 `ThreadWakeBatch` 取代。
它把 FIFO link 固定内嵌在 generation-bearing `ThreadCore` 中，batch 通过 owning
raw-Arc 和 external reap lease 保证节点在锁外 wake 完成前不会回收；加入 batch、
任意长度收集和锁外 drain 均不分配，重复加入同一线程会被合并。这对应 Linux v7.1
task-embedded `wake_q_node`，Starry futex 不再需要固定容量或超容量 heap fallback。

同一 futex syscall 还通过 `FutexContext` 只捕获一次当前 task/process；双地址的
requeue/wake-op 在一次 VMA snapshot 中解析两个 key，相同 private/shared domain 复用
同一个 table lease。x86_64 两 CPU TCG 的同核 OTHER futex 定向回归为 20,000/20,000
成功、`not_parked=0`，p50 142,151 ns；相对改造前同配置约 141,958 ns 属噪声范围，
说明这项改造主要收紧锁内分配和生命周期安全边界，并不是剩余延迟的主导项。

尚未消除的 futex 成本包括每个 key 动态创建的 `FutexEntry` 和进程级 `HashMap`
粗粒度查找。Linux v7.1 使用全局固定 futex hash bucket；下一阶段将 key 扩展为稳定
地址空间/共享后端身份并用固定桶替换 `FutexTable`。该步骤必须同时修正 `CLONE_VM`
但非 `CLONE_THREAD` 时 private futex 应按共享 mm 匹配的语义，不能只为降低查找成本
而保留错误的 per-process key 所有权。

qperf leaf 采样已能定位 CPU-local、guard、remote wake 和 schedule 小热点。完整
FP 模式原先会覆盖内核链接 rustflags，postprocess 也无法转发 `-cpu`；两项已由
单元红绿测试修复。FP 插件仍会在 x86_64 早期 `mmu_entry` 阶段尝试解栈并使宿主
QEMU SIGSEGV，因此现阶段不能把不完整 FP 报告用于归因；该诊断边界需要在插件
侧按采样窗口延迟启用，或对 early-boot FP 读取做显式有效性检查。

### 2026-08-03 重构前 remote-wake inbox 精确计数窗口

qperf 现在可以直接选择正式 Starry QEMU case，并复用同一构建配置、设备配置、基础
rootfs 和目标二进制。grouped case 的执行所有权被类型化为：

```text
GuestInit | ShellCommand | External
```

Starry 正式测试使用 `GuestInit`，Axvisor 使用 `ShellCommand`，qperf 使用 `External`。
旧 `/etc/profile.d` autorun 实现及其配置字段已删除；`External` 资产不会注入 grouped
runner，因此 init 不会在性能窗口前抢先启动 workload。x86 qperf 与正式 ostool 路径
统一默认 `q35`，不再意外退回缺少 ACPI MCFG 的 i440fx。

在 direct target-rq 重构前，x86_64、4 vCPU、正式
`qemu/test-ext4-inode-unique` 的 30.6237922 秒窗口内，指标增量为：

| 指标 | 增量 | 每秒 |
| --- | ---: | ---: |
| remote wake publication | 48,657 | 1,588.9 |
| remote wake inbox drain | 48,657 | 1,588.9 |
| lifecycle activation / owner enqueue | 41,648 / 41,648 | 1,360.0 |
| scheduler IPI send / consume | 38,251 / 38,251 | 1,249.1 |
| clockevent IRQ | 31,722 | 1,035.9 |
| context switch | 127,581 | 4,166.1 |

publication 与 drain 完全相等、send 与 consume 完全相等、activation 与 owner enqueue
完全相等，因此没有 remote message、IPI 或真实激活的永久丢失。publication 比
activation 多 7,009 次表示并发/重复 wake 在 lifecycle winner 处合并，不能解释为
lost wake。问题是结构性放大：96.7% 的 publication 遇到空 inbox 边，78.6% 最终发送
物理 scheduler IPI，而 Linux RT 的普通 remote wake 不需要 wake-list IPI。

本次 qperf 使用 leaf callchain；6,080 个样本均只有一层，不能据此伪造完整调用链。
单叶聚合仍显示 CPU-local current/area 查询约占 19.9%，guard/preempt 约占 8.3%，
timer/deadline 约占 19.5%。这些数据仅作为已经删除的 inbox 架构基线，不再作为当前
接口或指标名。重构后使用
`direct_wake_attempts / activations / enqueues / preemptions` 复测相同窗口；接受标准是
`activations == enqueues`，且只有 `preemptions` 对应的跨 CPU wake 产生 reschedule IPI。

### 2026-08-03 active-mm 回收风暴修复

同一 x86_64、4 vCPU、正式 `qemu/system` 资产和
`test-ext4-inode-unique` 3 秒窗口中，修复前 task-work class 指标为：

| 指标 | 窗口增量 |
| --- | ---: |
| worker pass / processed | 451 / 28,674 |
| worker yield / wait | 448 / 3 |
| 聚合 resource reclaim | 28,670 |
| exit callback / reap | 2 / 2 |

doorbell 只新增 13 次 publication，而 worker 却处理 28,670 次 resource，说明不是
28,670 个唤醒事件。确定性红测注入一个过期 readiness edge，并让 runtime 继续返回
`Busy`：旧实现单次 `service_deferred_task_work(64)` 稳定报告 64 项 progress，同一
address-space token 被反复 pop/push。修复后该 pass 报告 0，且只执行一次 destroy 和
一次 re-arm。

同时，普通 coroutine 的最后引用改为任务上下文直接释放；另一个确定性红测用
`SharedExecutor` 强引用数证明旧实现完成一个空 coroutine 后仍保留全局 reaper 引用，
新实现立即回到唯一 owner 引用。hard-IRQ 零分配、零释放、零 poll 测试继续通过，证明
IRQ 边界仍只做类型化 publication。

修复后相同 ext4 3 秒窗口为：

| 指标 | 窗口增量 |
| --- | ---: |
| worker pass / processed | 5 / 8 |
| worker yield / wait | 0 / 5 |
| coroutine reclaim / active-mm 成功回收 | 0 / 4 |
| exit callback / reap | 2 / 2 |
| scheduler IPI send / consume | 586 / 586 |

8 项 work 完全由 4 次真正完成的 active-mm 销毁、2 次 exit callback 和 2 次 reap
组成；没有饱和 pass、没有 yield 自旋、没有 coroutine backlog，IPI 仍保持一一消费。
这证明此前所谓“deferred reclaim 吞吐不足”实际是 `Busy` 状态被错误计成成功，而不是
需要放大 batch 或增加兼容 worker。首次 system 资产启动曾在 shell 前出现一次瞬时
停顿，随后同一构建的 GDB-capable boot 与 ext4 复跑均正常完成，未形成可复现缺陷。

### 2026-08-03 RISC-V pthread 栈复用与 mmu-gather

正式 `qemu/system/test-cargo-jobserver-wait` 在第二轮 eventfd+pipe epoll pthread 波次
稳定失败。修复前诊断日志证明 parent CPU 在 `0x57af0` 写入了新线程栈参数，而 child
CPU 从同一 VA 读取到旧页框中的随机值，随后把随机值当作入口跳转并 SIGSEGV。clone ABI
参数顺序与 Linux/musl 一致，根因不是局部线程创建参数，而是同一 `AddrSpace` 的线程
各自持有 runtime token，旧实现却按 token 统计 active CPU，并在仅本地清 PTE 后立即
回收 COW/file 页框。

修复采用上文的 shared-mm CPU tracker 与 typed `TlbGather`。最初若把每个普通 page
fault 都预登记为 shootdown，会把新 PTE 安装也变成同步 IPI，QEMU 立即出现数量级回退；
按 Linux 语义收敛后，只有 removal、replacement 和 permission downgrade 动态登记，
全新映射不 shootdown。大范围逐页 invalidation 同样改为架构阈值后的单次 full local
flush。最终同一命令完成 13/13 阶段，guest elapsed 2 秒、QEMU run 4.33 秒，并打印
`STARRY_GROUPED_TESTS_PASSED`。该证据同时约束正确性和“新 PTE 安装不得发送无意义
shootdown”的性能边界。

### 2026-08-04 当前 CPU owner snapshot

Linux v7.1 的 `__schedule()` 在抢占与 IRQ 已受控后，通过一次
`smp_processor_id()` 与 `cpu_rq(cpu)` 取得当前 rq；切换返回后的
`finish_task_switch()` 才重新执行 `this_rq()`，因为挂起的 continuation 可能已经在另一
CPU 恢复。TGOSKits 保留同样的两次捕获边界，但删除旧 `TaskRuntime` 中“先读当前 CPU
ID，再通过全局 `TaskSystem` registry 解析当前 remote endpoint，最后单独读取 owner
handle”的分裂接口。

新的 `CurrentCpuOwnerHandles` 在一个受 pin 保护的 runtime 调用中同时返回 CPU ID、
owner-only `CpuLocal` handle 和 Arc-backed `CpuRemote` handle。调度帧在切换前捕获一次，
raw switch 返回后重新捕获一次；trace 直接复用帧内 CPU ID。当前 CPU 的普通
reschedule 快路径仍保留专用 ID/remote 查询，避免为了只读一个字段而构造完整 owner
snapshot。四架构共享这一前端模型，差异只留在 CPU-local 寄存器读取与裸
`switch_to` 后端。

确定性回归把一次真实切换的 owner handle 捕获固定为 2 次，并要求通用 remote
registry lookup 为 0。旧实现稳定得到 `(2, 2)`，新实现得到 `(2, 0)`。这一步只报告
调用路径缩减，不把未复测的端到端延迟声明为性能提升；后续仍用同一 Q35/TCG
wakeup-latency workload 与 Linux PREEMPT_RT 对比。

同一阶段把两个可复制的 context handle 参数替换为 move-only `ContextSwitch`。ax-task
只能在调度决定已提交且 previous/next 非空、互异时构造该事务，ax-runtime 只能消费
一次。runtime 的 production 校验权威收敛到 `cpu-local::prepare_thread_switch()`：它在
同一 pin 下核对 current publication、previous CPU binding 并预绑定 next，失败时 token
析构回滚。RuntimeContext 与 TaskContext 的不可变构造关系只保留 debug assertion，不再
在每次 release switch 中重复验证。host 寄存器红测中，旧 prepare 路径读取
current-thread publication 2 次，新路径为 1 次；switch tail 仍以
`PreviousThreadBinding` 的 epoch 在 incoming continuation 中唯一清除 outgoing 绑定。

常见 scheduler safe point 进一步按 Linux 的一次 pre-switch rq transaction 收敛：先在
同一个 owner borrow 中检查 task deadline、remote work、`need_resched` 和当前线程状态；
只有确实存在到期 deadline 时才退出该事务，进入有界 expiry slow path，再重新取得
owner。旧实现即使没有 deadline 也会先进入 deadline helper、释放 owner，然后再次取得
owner 做调度，连同 switch tail 一次真实切换固定产生 3 次 owner claim；确定性红测在旧
路径得到 3，新路径固定为 2，即切换前一次、返回后的 switch tail 一次。这个合并不把
owner guard 跨越 timer wake 或 context switch，也不改变 deadline batch/backpressure
语义。

同一 Q35/TCG x86 wakeup-latency 完整组通过正式成功标志。相对前一检查点，OTHER 同 CPU
p50 从 124.292 微秒变为 122.433 微秒，FIFO 同 CPU p50 从 132.665 微秒变为
120.195 微秒；跨 CPU 与 timer 场景没有出现功能回退。单轮 TCG 结果只证明该检查点未
引入明显回退，不能单独证明稳定加速；绝对延迟仍高于同宿主 Linux PREEMPT_RT，后续继续
审计每次切换的 balance、clockevent 重编程和远程唤醒放大。

### 2026-08-04 rq balance 触发与 move-only 迁移选择

Linux v7.1 的 `__schedule()` 不把 SMP balance 作为每次 context switch 的固定尾部：普通
切换只完成本 rq 的 pick/commit，周期 balance 由 scheduler tick 和调度类条件触发，idle
进入时才执行 idle balance；`resched_curr()` 只发布本地 flag 或必要的远程 IPI。TGOSKits
原 `finish_owner_selection()` 则无条件进入 `balance_after_schedule()`，即使没有 idle pull、
RT/Deadline overload 或 Fair 周期期限也执行一遍通用 balance 分派。

新模型先从 owner 已发布的 coherent rq summary 判断是否存在三类显式工作：idle pull、
RT/Deadline push、Fair periodic balance。普通切换直接跳过 SMP balance；到期 Fair deadline
仍由 task clockevent 唤醒 scheduler safe point，因此不依赖后续偶然 IRQ。确定性红测中，
单 CPU 普通 owner selection 的 balance pass 从 1 降为 0。

迁移选择改为 move-only `OwnerBalanceSelection`：同一 source-rq 扫描同时选定一个候选和
它的最佳 destination，commit 只能消费该选择一次。旧 Fair 路径会为 3 个低负载目标各
扫描一次 source rq，随后 transfer 再扫描一次；分步收敛的红测先固定了 2 次变 1 次，
四 CPU 红测进一步固定 3 次变 1 次。RT/Deadline push 同样复用一次候选选择，不再按每个
目标重复扫描。commit 在真正 detach 前重新核对 target online/scheduler-ready、affinity、
sleep timer、Deadline root-domain coverage 和 placement；并发 affinity 更新的红测在旧
路径错误迁移到已禁止 CPU，新路径返回 retry 且保留 source rq 所有权。

同一 x86 Q35/TCG wakeup-latency 完整组再次通过。相对前一检查点，8 个场景的 p50 中
5 个下降、3 个上升：OTHER 同 CPU/跨 CPU thread/跨 CPU process 分别为
120.047/111.541/115.152 微秒，absolute timer 为 163.929 微秒；FIFO 同 CPU/跨 CPU
thread/跨 CPU process 分别为 119.200/114.416/124.039 微秒，absolute timer 为
160.585 微秒。guest clock-pair 最小成本同时从 26.205 上升到 27.326 微秒，且 FIFO
仍出现约 50 毫秒离群点，因此这轮只判定无系统性回退；balance 改造的主要证据是上述
确定性调用次数和竞态不变量，不把混合的单轮 TCG 分布声明为稳定性能提升。

### 2026-08-04 当前线程 affinity 与 Fair 单实体快路径

Linux v7.1 的 `yield_task_fair()` 在 `rq->nr_running == 1` 时可以直接返回，但 affinity
更新并不依赖后续 yield 推进：`__set_cpus_allowed_ptr_locked()` 总是进入
`affine_move_task()`；运行中任务由 `migration_cpu_stop` 强制离开旧 CPU，已排队任务则在
rq 锁内直接迁移。`migration_pending`、`on_rq` 和目标 CPU 的重校验共同保证 Fair 的单实体
快路径不会让任务永久留在 affinity 已排除的 CPU。

当时的 affinity 迁移没有独立 stopper 线程，当前任务的同步 affinity 接口在持有 scheduler
baton 时发布 `migration_target`，随后通过一次 owner schedule-out 完成迁移。因此 Fair
单实体快路径不能只判断本地 rq 为空；它还必须确认 placement 仍是本 CPU 的
`Running + on_cpu`、affinity 仍包含 owner，且没有待提交的 migration。该条件收敛为
`ThreadPlacementState::can_continue_running_on()`，所有条件满足时才允许 self-dispatch；否则
进入统一 schedule-out/switch-tail 事务，迁移切换记录为 `SwitchReason::Migrated`。后续为
Starry `stop_machine` 新增的 per-CPU stopper 是独立的内核停止类，不改变普通 affinity 的
owner schedule-out 协议。

确定性红测复现了 CI 的最小状态：两 CPU、当前 CPU 上只有一个 Fair 线程，把 affinity
收窄到另一 CPU 后调用 `yield_current()`。旧实现返回 self-to-self `Yield`，新实现选择本地
idle 并提交 `Migrated` handoff。对应 RISC-V 四 CPU QEMU `memtest` 已通过，包含 parallel
allocation worker 逐 CPU pin、迁移和 cross-CPU free，正式结果为 `1/1 case(s) passed`。

独立 `task-affinity` 压力项还暴露出第二个结构问题：`ax_set_current_affinity()` 先通过
`thread_affinity()` 复制整个 mask，只为取得 topology width；核心更新随后又通过全局
`TaskSystemState` registry 查回当前线程。八个 worker 并行 pin 时，GDB 观察到四个 vCPU
同时排在同一 registry ticket lock 上，调度无关的全局身份表成为当前 rq placement 的串行
瓶颈。Linux 的当前任务 affinity 事务由 task metadata 与 owner rq 串行，不经过全局 PID/
task registry。

新路径公开只读、固定的 `cpu_topology_len()`，上层不再查询当前线程 mask；
`set_current_affinity()` 直接使用 scheduler baton 已经稳定持有的 `CpuLocal::current_core`，
只锁 root-domain 和该线程的 scheduler state，并通过 CPU remote snapshot 选择目标。topology
width 不使用 online count，避免 hotplug 后改变 affinity mask ABI。RISC-V 四 CPU
`task-affinity` 从修复前超过 30 秒仍无结束标志，收敛为 guest 内 44 毫秒完成，runner 正式
结果为 `1/1 case(s) passed`。

### 2026-08-04 WaitQueue generation 与锁外 predicate

Linux v7.1 的 `prepare_to_wait*()` 只在 waitqueue spinlock 内修改 waiter 链表，调用方的
条件检查和 `schedule()` 均在该内部锁外执行；唤醒侧先在受限临界区选择 task，再通过
`wake_q` 于外部完成实际 wake。PREEMPT_RT 的 rtmutex handoff 同样先提交 owner/waiter
元数据，再在 raw lock 外唤醒，不允许内部 scheduler-sensitive lock 调用任意上层闭包。

旧 `WaitQueue` 恰好相反：`wait_until*()` 在 `PreemptTicketLock<VecDeque<_>>` 内执行任意
predicate，`notify_one_with()` 也在该锁内执行上层 handoff。Starry TPU、perf CPU worker、
ax-runtime serial control 等 predicate 会取得各自的任务态锁；生产者通常先修改该任务态
状态，再调用 `notify_*()`。这形成 `waiters -> domain state` 与
`domain state -> waiters` 的确定性反向锁序，并把分配、PiMutex 阻塞或重入调度的能力带进
raw/preempt 临界区。

新协议为每个 WaitQueue 增加单一 `notification_generation`。等待者先 Acquire 读取
generation，再在内部锁外检查 predicate；进入 waiter lock 后只比较 generation，若有通知
跨过检查/入队窗口便退出并重新检查，否则提交 waiter 与 park ticket。通知方先取得 waiter
lock，在同一事务中 Release 推进 generation 并移除目标，随后完全释放内部锁再执行 wake。
`notify_one_with()` 允许先选 waiter、后由任意闭包发布条件，本质违反 publish-before-notify，
因此直接删除，不提供兼容入口。上层必须先发布领域状态，再调用普通 notify；ax-api wake
返回实际选择的 waiter 数，测试和需要精确计数的调用方不再借助锁内回调探测队列。

两个最低层红测在旧实现中稳定失败：elapsed deadline predicate 无法重新取得 waiter lock，
notification handoff callback 也观察到 waiter lock 被占用；前者作为持续行为回归，后者驱动
危险 API 整体删除。loom 穷举覆盖 predicate check、generation publication、waiter commit
和 notify selection 的全部交错，保证已提交 waiter 必被选择，窗口内通知则强制重试。
x86_64 四 CPU QEMU
`task-wait-queue-remote-wake` 正式通过，guest 13 毫秒，runner `1/1 case(s) passed`。

### 2026-08-04 Park、deadline 与 PI 单 owner 事务

Linux v7.1 的 `__schedule()`、`try_to_wake_up()` 和 rtmutex 慢路径都以当前 task 状态与
目标 rq/PI 元数据的单次受控事务为边界。调用方不会先复制一份 current task，再释放保护，
随后为同一次入睡准备反复取得当前 rq。旧 facade 却把 current identity、`Parking`
publication、deadline owner 校验和 PI policy drain 分成多个 `runtime_current_cpu()` 调用；
每次调用都会重新关闭 IRQ、读取 CPU-local handles 并 claim `CpuLocal` owner gate。

新的 `begin_current_park()` 在一次 IRQ-off owner borrow 内同时复制 generation-bearing
`ThreadHandle` 并执行 `prepare_park()`。`PreparedCurrentPark` 对上层只额外暴露受限的
`ThreadWakeHandle`；`WaitQueue` 在自己的 waiter lock 内取得该 move-only transaction，
发布 wake capability 后在锁外 commit，不再先保存完整 scheduler handle 再走旧的
`prepare_current_park()` 旁路。sleepability 必须在取得 waiter 的非睡眠 preempt lock 前
验证；不可逃逸的 blocking permit 只授权紧随其后的这次 park publication。测试 runtime
现在与 production 一样检查普通 preempt depth，避免 host 测试把生产 runtime 会拒绝的
锁内验证误判为安全。deadline arm/cancel 同样在一个 owner borrow 内完成 current 校验、
CPU ownership 校验、heap mutation 与下一物理 deadline 计算。

PI 慢路径把 current waiter 校验、owner policy update drain 和 `Parking` publication 合并为
一次 owner transaction；token 已经 selected/granted 时仍在释放 owner gate 后返回，实际
block/wake 继续位于 PI metadata 和 rq 锁之外。旧测试专用 park helper 不再进入 production
接口，不保留双轨兼容用法。

最低层计数红测直接统计 `CpuLocal` owner claim：

| 路径 | 修复前 | 修复后 |
| --- | ---: | ---: |
| `begin_current_park()` identity + Parking publication | 2 | 1 |
| WaitQueue waiter publication | 2 | 1 |
| park deadline owner validation + arm | 2 | 1 |
| PI current validation + policy drain + park publication | 3 | 1 |

这些测试约束的是所有调用者共享的事务边界，不以某次 TCG 延迟波动替代正确性证据。端到端
性能使用同一 Q35/TCG wakeup-latency workload 复测。第一次运行的 guest clock-pair 最小
成本从基线 26.753 微秒上升到 35.131 微秒，8 项原始 p50 有升有降，不能归因给代码；立即
复跑时 clock-pair 为 25.437 微秒，QEMU 时间从基线 78.23 秒降到 76.75 秒，8 项 p50 全部
下降：OTHER 同 CPU、跨 CPU thread、跨 CPU process、timer 分别下降 4.2%、2.5%、3.4%、
6.0%，FIFO 对应四项分别下降 4.5%、7.0%、4.4%、5.8%。其中 same-CPU OTHER 从
123.143 降到 117.913 微秒，OTHER timer 从 227.770 降到 214.212 微秒。

两次都打印正式 `WAKEUP_LATENCY_PASSED`，第二次同基线的 clock-pair 更可比；但单宿主 TCG
复跑只能证明当前检查点没有性能回退并与 owner-claim 降低方向一致。绝对延迟仍为 Linux
PREEMPT_RT 的数倍，后续必须继续审计 current handle、switch tail 和 clockevent 固定成本，
不得把这组相对改善描述为已经达到 Linux RT 水平。

### 2026-08-04 current identity 与 PI current token

Linux 的 `current` 是当前执行上下文直接持有的 task identity；只读取 current 或
`TIF_NEED_RESCHED` 不会取得 rq lock，也不会克隆 task 引用。TGOSKits 原
`current_thread_id()` 和 `current_cpu_needs_resched()` 却统一进入 IRQ guard、读取三元
CPU owner handles、claim 可变 `CpuLocal` owner gate，导致 trace、syscall、PI 和 executor
等所有只读调用者与真正的 rq mutation 竞争。

新的只读路径仅用一个短 task-migration pin，从 `CpuRemote` 的 Acquire publication 复制
generation-bearing `ThreadId` 或 sticky reschedule word；不关闭 IRQ、不取得 mutable owner
gate，也不访问全局 registry。运行时 hook 即使位于一个现存 owner transaction 内，也可以
读取这两个独立 publication；完整 `ThreadHandle` 仍必须经过 owner gate，不能借只读接口
绕过 Arc 生命周期和 `CpuLocal` 可变借用不变量。最低层计数红测中，两种只读查询的 owner
claim 都从 1 降为 0，并各保留一次必要的 migration pin。

PI mutex 进一步使用不可复制、不可跨线程传递的 `CurrentThreadToken`。ax-sync 在 owner-word
fast path 捕获一次当前身份，随后把同一 token 传给 waiter 注册、跨 park 恢复后的 ownerless
claim；ax-task 每次 waiter 状态修改仍用 token 内的 generation-bearing ID 在 registry/PI
metadata 下重新验证。这样不会把可伪造的裸 `ThreadId` 暴露成安全任务态 API，也不会为了
“确认还是同一线程”克隆完整 `ThreadHandle`。确定性红测中，PI slow registration 的完整
current-handle/owner snapshot 从 1 降为 0，注册阶段的 task-preempt transaction 从 2 降为
1；剩余一次属于 PI metadata lock 本身。

解锁权限单独来自 `lock_api::RawMutex::unlock` 的 unsafe owner contract，而不是再次捕获
current。`PiMutexCore::try_release_owned()` 只能在该 contract 下调用：无争用时直接对 owner
word 做 release-CAS；发现 waiter bit 后返回 owner word 中的 generation-bearing identity，
调用方从 scheduler deboost、ownerless handoff 到锁外 targeted wake 全程保持 preempt pin。
这对应 Linux v7.1 `__rt_mutex_unlock()` 的 owner-word fast path，以及 slow path 在
`wait_lock` 内完成 deboost/next-owner publication、锁外 `wake_q` 的顺序。区别是 Linux 用
廉价的 `current` 再校验 owner，而 Rust raw-mutex 边界已经把“调用者持有该锁”作为 unsafe
前置条件；重复进入 runtime current hook 只会给每次 unlock 增加一次 migration pin，并不能
把违反 unsafe contract 的调用变成安全调用。需要代理任意线程的模型接口因此改名为
`try_*_for_thread` 并标记 unsafe，生产任务态获取只能使用 typed current token。

对应确定性红测在旧实现中执行 128 次无争用 lock/unlock 会进入 256 次 task-preempt guard；
新路径只在 lock 时捕获一次 current，降为 128 次，IRQ facade 始终为 0。contended unlock
测试继续断言 waiter selection 先于 claim、deboost 先于 wake，且物理 owner 只能由被选
waiter提交。这个收益是固定成本消除，不依赖 QEMU 主机噪声；端到端性能仍以随后同配置的
wakeup-latency 检查点为准。

该检查点的 x86_64 Q35/TCG 完整组再次打印 `WAKEUP_LATENCY_PASSED`。guest clock-pair 为
28.983 微秒，相对上一检查点的 27.342 微秒慢 6.0%；QEMU workload 为 81.30 秒，相对
79.87 秒慢 1.8%。OTHER 四项 p50 为 129.489、118.924、120.051、219.145 微秒，FIFO
四项为 129.409、118.168、122.082、225.409 微秒。由于 host TCG 时基不同，不能把 raw
p50 直接当作代码回退；与 25.437 微秒时基的最近可比检查点归一化后，已知的 OTHER 同核
futex 和 timer 分别改善约 3.6% 与 10.2%。本检查点据此只确认完整功能通过、固定 guard
操作数减半且未观察到归一化回退；绝对值仍是 Linux PREEMPT_RT 的数倍，不能宣称已达到
同一性能水平。

本轮同时重新核对 Starry futex 锁边界。Linux v7.1 `futex_wait_setup()` 在 hash-bucket lock
内二次检查用户值、设置 waiter state 并入队，释放 bucket 后才 `schedule()`；wake 在同一锁
内摘除 waiter 并 release-publish 失效，锁外执行 `wake_up_q()`。PREEMPT_RT 下该
`spinlock_t` 会成为基于 rtmutex 的可睡眠锁。Starry 对应 bucket 已使用 `PiMutex`，且只在
锁内执行 nofault condition、park publication 和 waiter insertion，实际 park/wake 都在锁外，
因此保持现状。ax-task 的内部 blocking permit 不公开给 futex：公开它会允许 arbitrary raw
或 preempt lock 绕过 `might_sleep()` 风格校验，反而破坏安全边界。

x86_64 Q35/TCG 完整 wakeup-latency 复测通过正式 `WAKEUP_LATENCY_PASSED`，但该轮 guest
clock-pair 从上一可比轮的 25.437 微秒升到 27.342 微秒，QEMU 时间从 76.75 秒升到
79.87 秒，不能直接把原始 p50 波动归因给代码。按 clock-pair 比例归一化后，OTHER 同 CPU/
跨 CPU thread 分别约改善 4.0%/2.3%，跨进程持平；FIFO 四个 futex/timer 项在 -0.8% 到
+2.9% 内，OTHER timer 约慢 7.1%。立即热缓存复跑的 clock-pair 又升到 32.261 微秒，按
“先看时基、不可比即停止”的规则在第二项开始前终止，没有用更慢宿主样本制造结论。当前
检查点的确定性收益是 owner claim 和 PI preempt transaction 减少；timer 差异留给独立
clockevent generation/early-fire 阶段验证，仍不宣称已达到 Linux RT 绝对性能。

### 2026-08-05 CPU-owner per-CPU 直接访问

qperf leaf 样本中 `cpu_local::register::current_thread()`、`current_area()` 与 guard enter/exit
反复出现。完整调用链显示 `RuntimeGuardState` 和 `CPU_REMOTE_HANDLE` 都是
物理 CPU 所有状态，但旧 `with_scheduler_current[_mut]()` 每次先读 current
thread，再从 `CurrentThreadHeader::cpu_area_base()` 反查 per-CPU 符号。这与 Linux
v7.1 scheduler/IRQ 状态直接使用 `raw_cpu_ptr()`/`this_cpu_ptr()` 的 owner 边界不同。

首个确定性红测统计架构寄存读：旧实现每次 CPU-owner 访问为
`cpu_base=0, current_thread=1`。第一版直接使用 `CpuAreaRef` 后虽变为
`1/0`，第二个红测仍稳定观察到每次重建完整 area 并重复检查
bootstrap header/identity 一次。最终实现以不可逃逸的 `SchedulerCpuArea`
token 只选择架构 CPU base，宏生成符号只能在 HRTB callback 内构造 typed
pointer。最终操作数为 `cpu_base=1, current_thread=0,
initialized_area_validations=0`；常规 `CpuPin` 仍保留 current-task 与 area 的双源完整
校验，不因热路优化降低通用安全边界。

与远端上一检查点相同的 x86_64 Q35/TCG 完整组打印正式
`WAKEUP_LATENCY_PASSED`，QEMU workload 由 81.30 秒降到 79.19 秒（-2.6%）。
guest clock-pair 由 28.983 微秒降到 25.877 微秒，因此原始 p50 只作阶段趋势：

| 场景 p50 | 上一检查点 | 当前 | 单轮变化 |
| --- | ---: | ---: | ---: |
| OTHER 同 CPU futex | 129.489 us | 122.544 us | -5.4% |
| OTHER 跨 CPU thread | 118.924 us | 115.928 us | -2.5% |
| OTHER 跨 CPU process | 120.051 us | 115.597 us | -3.7% |
| OTHER absolute timer | 219.145 us | 222.061 us | +1.3% |
| FIFO 同 CPU futex | 129.409 us | 127.012 us | -1.9% |
| FIFO 跨 CPU thread | 118.168 us | 117.093 us | -0.9% |
| FIFO 跨 CPU process | 122.082 us | 117.515 us | -3.7% |
| FIFO absolute timer | 225.409 us | 184.774 us | -18.0% |

首次基线恶化到 33.220 微秒的复跑按规则中止，没有混入表格。相对那个仍重复
area 校验的中间版（clock-pair 26.872 微秒），最终版八项 p50 全部下降，
其中 OTHER/FIFO timer 分别下降 12.8%/29.8%。这与确定性删除完整 area 重验的
方向一致，但绝对延迟仍明显高于 Linux PREEMPT_RT，不宣称已达到同一水平。

### 2026-08-05 唤醒基准的 syscall 测量边界

重新沿用户态取时调用链核对后发现，旧基准使用 libc `clock_gettime()`：同一份静态
源码在 Linux 上可以由 vDSO 完成，而 StarryOS 当前必须进入内核。因此上文历史表中的
Linux RT 与 StarryOS 绝对 p50 混入了不同的取时路径，不能继续用来判断两者是否已经达到
同一水平；这些数字只保留为发现该测量缺陷前的历史趋势。

当前基准强制通过 raw `SYS_clock_gettime` 读取每个 producer/consumer 时间戳，metadata
也用 raw `SYS_clock_getres` 并发布 `clock_read=raw_syscall`。确定性红测把真实 `stats.c`
与 `--wrap=clock_gettime` 的故障注入链接：旧实现必然进入被替换的 libc 符号并以
`ENOSYS` 失败，新实现绕过 libc 后同一测试通过。这个测试随 `prebuild.sh` 执行，避免将来
无意恢复 vDSO/Starry syscall 不对称。

qperf 的 monitor 每次运行只消费第一对 start/stop marker。完整 workload 因此只发布唯一的
`WAKEUP_LATENCY_PROFILE_START/DONE`：策略能力探测和 metadata 位于窗口外，全部已选择
场景位于窗口内。逐场景 marker 则移动到策略/亲和性配置之后、正式 benchmark 前，以及
benchmark 返回后、排序和报告前；它只用于一次 QEMU 运行选择单个 case 时收窄调用链。
下一轮 Linux RT/StarryOS 绝对对比必须重建同源 initramfs，并同时观察
`clock_read=raw_syscall` 与正式通过标志，不能沿用旧产物或旧绝对表。

在同一宿主、Q35/TCG、2 vCPU、512 MiB 下重建 Linux v7.1 PREEMPT_RT initramfs 后，
Linux 与当前 StarryOS 均打印 `clock_read=raw_syscall` 和正式通过标志。新的 p50 如下，
单位为微秒：

| 策略与场景 | Linux RT | StarryOS | 倍数 |
| --- | ---: | ---: | ---: |
| OTHER，同 CPU 线程 | 15.068 | 129.410 | 8.59x |
| OTHER，跨 CPU 线程 | 25.488 | 120.884 | 4.74x |
| OTHER，跨 CPU 进程 | 28.276 | 122.579 | 4.34x |
| OTHER，绝对 timer | 92.019 | 195.493 | 2.12x |
| FIFO:80，同 CPU 线程 | 11.912 | 128.747 | 10.81x |
| FIFO:80，跨 CPU 线程 | 21.859 | 119.713 | 5.48x |
| FIFO:80，跨 CPU 进程 | 23.739 | 123.540 | 5.20x |
| FIFO:80，绝对 timer | 126.121 | 181.625 | 1.44x |

Linux/Starry 的 raw clock-pair 最小值分别为 0.693/28.407 微秒。这个固定取时成本已经
进入每个样本，但不能从分位数中猜测性相减；它说明下一轮 profile 必须分别观察 syscall
entry/return、futex wake、rq transaction 和 switch tail，不能把全部差值归给 scheduler。
相对上一检查点，Starry clock-pair 慢 9.8%，四个 futex p50 变化为 +1.4% 到 +6.0%，
两个 timer p50 下降 1.7%/12.0%；QEMU 运行由 79.19 秒变为 81.39 秒。当前提交没有修改
scheduler，且取时基线同步变慢，因此这些单轮变化只证明修正后的协议可完整运行，不作为
调度性能回退或改善声明。可信的绝对结果确认当前仍未达到 Linux RT 同一水平，最大固定
差距位于同 CPU futex/FIFO 路径，下一阶段优先剖析该调用链。

### 2026-08-05 x86 LinuxCurrent 用户 TLS 所有权

精确 case marker 的 qperf 运行使用 `qemu/system` staging、2 vCPU 和 leaf callchain，
FIFO 同 CPU futex 的 25.90 秒 workload 内取得 2,557 个样本并正式返回 `result: ok`。
窗口内没有 remote IPI 放大；最高占比落在 current identity、owner pick、runqueue enqueue、
IRQ/preempt guard 和时间换算。第一次用默认单 CPU rootfs 与 FP callchain 的运行在进入 shell
前由 QEMU plugin SIGSEGV，报告明确为 `incomplete`，其中固件地址样本不进入任何性能结论。

沿 syscall 汇编继续审计后发现更基础的寄存器 owner 错配。Starry final image 明确使用
LinuxCurrent，`ax-cpu` 也禁止 `uspace + tls`；内核不拥有 FS 寄存器。但旧 x86 用户陷入仍在
每次 syscall/用户异常入口执行 3 次 `RDMSR` 和 1 次 `WRMSR`，返回用户前再执行 2 次
`WRMSR`，其中一读一写只为保存并恢复不存在的 kernel FS。与此同时，`UserContext.fs_base`
在用户执行期间还被临时复用为 kernel continuation stack，迫使入口重新读取用户 FS。

Linux v7.1 `entry_SYSCALL_64`（`arch/x86/entry/entry_64.S:87-121`）不在普通 syscall
入口读写 FS/GS MSR。非 FSGSBASE 的上下文切换路径在
`arch/x86/kernel/process_64.c:231-292` 对 selector 0 的常见 64 位线程直接信任已保存 base，
明确以避免热路径 `RDMSR`；真正的 FS/GS owner 切换收敛在 `__switch_to()` 的
`save_fsgs()`（同文件 `610-632`）。

当前 x86 LinuxCurrent 路径据此完成以下破坏型收敛：

- `UserContext.fs_base/gs_base` 是禁用 `CR4.FSGSBASE` 时唯一的用户 TLS 软件镜像；
- 独立的 `kernel_stack_pointer` 保存用户执行期间的 kernel continuation，不再复用 FS 字段；
- 用户陷入后的 Rust 内核保持用户 FS live，只通过 `SWAPGS` 恢复 CPU area；入口不再执行
  任何 TLS `RDMSR/WRMSR`；
- 返回 ring 3 前仍按 `UserContext` 写一次 FS 与 inactive GS，保证迁移、阻塞、clone、ptrace
  和 `arch_prctl` 后的值正确。后续只有在建立 generation-bearing per-CPU user-TLS binding
  后，才可继续把这两次写收敛到真实 owner/值变化，不能无条件省略。

确定性红测直接约束汇编边界：旧实现缺少独立 continuation 字段，trap-to-Rust 段含 3 次
`RDMSR` 与 1 次 `WRMSR`，测试稳定失败；新实现该段读写 MSR 均为 0，user-return 段恰好
2 次 `WRMSR`。行为验证中，`ARCH_SET/GET_FS/GS` 7/7、clone/pthread/`CLONE_SETTLS`
21/21、futex wake-op SMP 80,000 次和 AVX context switch 均命中正式 grouped pass 标志。

同一 raw-syscall x86 Q35/TCG 完整复跑结果如下：

- clock-pair `28.407 -> 25.458` 微秒（-10.4%），QEMU `81.39 -> 78.11` 秒（-4.0%）；
- OTHER 同核/跨核线程/跨核进程 futex p50 分别下降 4.9%/6.5%/6.8%；
- FIFO 同核/跨核线程/跨核进程 futex p50 分别下降 5.3%/4.7%/7.1%；
- OTHER/FIFO timer p50 反而上升 12.0%/22.4%。该改动不触及 clockevent/deadline，timer
  的相反变化保留为独立抖动或后续 finding，不用 futex 改善掩盖，也不归因给 TLS 修改。

修正后的 Starry/Linux RT p50 倍数为：OTHER 同核 8.17x、跨核线程 4.44x、跨核进程
4.04x、timer 2.38x；FIFO 同核 10.24x、跨核线程 5.22x、跨核进程 4.83x、timer 1.76x。
固定 syscall 成本下降后仍未达到同一水平，下一阶段继续处理剩余 user-return MSR owner
publication，以及 qperf 已显示的 current identity、owner pick 和 guard 固定成本。

#### per-CPU 用户 TLS 物理 owner

第一版继续照搬 Linux `__switch_to()` 的位置，在每次 scheduler context switch 保存和安装
用户 FS/GS。确定性边界测试虽然通过，但同一 benchmark 立即暴露了设计错误：Starry 的
kernel worker 与 idle 不使用用户 TLS，这种做法仍会在用户线程与内核线程之间无条件清零、
恢复 MSR。OTHER 同核/跨核 p50 分别从 123.100/113.047 微秒退化到
125.893/118.823 微秒（+2.3%/+5.1%）。该中间实现未提交，不能作为兼容路径保留。

最终模型把“任务期望值”和“CPU 物理值”分离：

- `UserContext` 继续独占任务期望的 FS/GS base；
- `CpuUserTlsState` 位于 CPU area 的 architecture reserve，独占当前 CPU 已安装的物理镜像
  及非零 generation；只允许本 CPU 在 IRQ 关闭时访问；
- scheduler switch 不搬运用户 TLS，kernel worker/idle 也不清零 TLS；
- `UserContext::run()` 在最后一次返回 ring 3 的边界比较 task image 与 CPU image，只对变化
  的寄存器执行 `WRMSR`，更新完成后再发布 generation；
- syscall/异常入口和 `enter_user` 汇编均不再含 TLS `RDMSR/WRMSR`。同一用户 owner 的稳态
  syscall 因而从上一检查点的 2 次 `WRMSR` 收敛为 0，迁移、clone、`arch_prctl` 或首次
  用户返回仍会正确安装变化值。

这不是复制 Linux 的函数位置，而是复用 Linux 的单一物理 owner 原则。Linux 同时在 kernel
task 中使用统一的 `thread_struct` 切换边界；TGOSKits 的 LinuxCurrent image 已把 user context
与 kernel scheduler thread 分开，因此最窄且不会干扰内核线程的 owner 交接边界是 ring-3
返回点。

确定性红测先要求 `prepare_switch_to()` 不得安装用户 TLS、`enter_user` 的 MSR 读写数必须为
0，并要求 CPU-local generation 与 changed-only 比较同时存在；它在中间实现上稳定失败，最终
实现通过。ax-cpu host test 覆盖未初始化时写两个寄存器、单字段变化时只写一个、相同 owner
不写寄存器。x86 `arch_prctl-tls` 与 `clone-tls` 两组 QEMU 均正式通过：FS/GS 7/7、
clone/pthread/`CLONE_SETTLS` 21/21、futex wake-op SMP 80,000/80,000 和 AVX context
switch 4/4。

最终完整 benchmark 的 QEMU 用时为 78.53 秒，上一检查点为 78.11 秒（+0.5%，视为持平）。
raw clock-pair 从 25.458 变为 27.433 微秒（+7.8%）；八个场景的 raw p50 变化范围为
-1.6% 到 +4.2%，但按各次 clock-pair 归一后全部改善 3.3% 到 8.7%。原始 p50 与 Linux RT
的倍数仍为：OTHER 8.10x/4.51x/4.18x/2.42x，FIFO 10.19x/5.33x/5.04x/1.73x。
因此本检查点只证明消除了稳态用户返回的架构性 MSR 开销；绝对性能仍未达到 Linux RT
同一水平，后续必须继续审计 current identity、guard 与 wake/rq transaction，不能把
clock-normalized 改善表述成已经完成性能对齐。

### 2026-08-05 current identity 的本地执行上下文所有权

旧 `current_thread_id()` 虽然只需要调用线程自己的 generation-bearing identity，仍先取得
`CpuRemote`，再读取该远程 runqueue publication endpoint 的 `current_thread`。这把 Linux
本地 `current` 与远程 `rq->curr` 混成同一接口：本 CPU 的 syscall、trace 和任务态资源检查
都要经过本来只应服务 remote observer 的共享缓存线。

Linux v7.1 的四架构模型在语义上统一为“架构寄存器或 cache-hot per-CPU 槽直接选择当前
`task_struct`”：x86 的 `get_current()` 读取 `current_task`，AArch64 读取 `sp_el0`，RISC-V
固定用 `tp`。runqueue 的 `rq->curr` 仍归 scheduler owner，不能反过来充当本地 current
identity 的事实源。

本轮按该边界做破坏型收敛：

- `TaskRuntime::current_thread_identity()` 明确只允许读取架构 current-thread register 选中的
  本地执行上下文；`CpuRemote.current_thread` 只保留 remote rq snapshot 语义；
- `CurrentThreadHeader` 新增一次性 `RuntimeThreadCookie`，在 scheduler generation 进入 runqueue
  前绑定；零值只表示未绑定的 bootstrap header，第二次绑定返回原 owner；
- cookie 使用原 header reserved 空间，仍位于同一个 64B current cache line；既有
  CPU-base、architecture scratch、preempt-state 偏移和四架构汇编 ABI 不变；
- ax-runtime 不再从 offset-zero header 追到 `RuntimeContext` 的第二缓存线，也不再另存一份
  `thread_identity`；读取只包含 current pointer 与同缓存线的 immutable cookie；
- ax-task 的未初始化错误仍只在零 cookie 冷路径查询 `TaskSystem`，正常 current 快路径没有
  registry、runqueue、IRQ owner 或远程 handle 访问。

确定性红测先要求 cookie 在旧 header 上不存在并稳定编译失败；实现后验证一次绑定成功、
重复绑定拒绝且 header 仍为 64B。facade 操作计数同时要求一次 `current_thread_id()` 的
`current_cpu_remote_handle` 读取从 1 降为 0，CPU owner claim 保持 0，迁移 pin 保持 1。

完整 wakeup benchmark 不是这条接口的归因测试：Starry timer/futex 的阻塞主路径使用
`current_thread_handle()`，不会调用新的 identity hook。诊断运行中 OTHER timer p50 曾从
218.9 微秒漂移到 248.8 微秒，而 futex case 同时有升有降；该运行已按规则提前停止，不能
把 QEMU/TCG clockevent 抖动归因给 cookie，也不能用它声称 identity 已达到 Linux RT 性能。
本检查点的确定性能效证据仅是远程 endpoint 读取 1→0 和跨缓存线追踪 1→0；下一阶段应把
Starry `current_user_task()` 从 registry-backed `current_thread_handle()` 收敛为生命周期安全的
本地 current extension capability，才会覆盖 futex、nanosleep 和大多数 syscall 热路径。

### 2026-08-05 syscall current capability 的显式传播

Linux v7.1 的 syscall 在当前 `task_struct` 的执行上下文中完成。四架构的 `current` 入口不同，
但生命周期规则一致：同步 syscall 路径可直接借用当前任务；只有把任务发布到 wake queue、
callback、异步 worker 或其他 CPU 时才通过 `get_task_struct()` 增加强引用。阻塞后返回并不要求
重新查找 current，因为被调度回来的仍是发起 syscall 的同一个任务。

Starry 用户线程入口原本已经在整个用户执行循环中持有强 `UserTaskRef`，其中
`ThreadHandle` 同时固定 scheduler record 与 Starry extension 的生命周期；但旧
`handle_syscall(&Thread, ...)` 丢弃了这项能力。syscall 子树中的 171 个调用点随后重新调用
`current_user_task()`，每次都要取得可变 CPU owner、克隆当前 `ThreadHandle` 并重新校验
extension。`poll`、`wait` 等路径甚至会在同一个 syscall 中重复该过程。

本轮按 Linux current 的边界整体迁移，不保留隐藏 getter 兼容层：

- 用户执行循环把同一个 `&UserTaskRef` 交给 `handle_syscall`；
- 只有实际依赖当前任务的 syscall 和辅助函数才显式接收这项能力，不需要 current 的 syscall
  保持原签名；
- sleep、futex、poll、select、epoll、AIO、signal wait、waitpid 和 IPC 阻塞路径在调度前后都
  借用同一个强 capability，不重新查询 runqueue owner；
- 凭据、PID、namespace、地址空间和调度策略查询从该 capability 收窄到 `&Thread` 或
  `ProcessData`，不得保存跨任务的裸 extension 指针；
- syscall 目录内的 `current_user_task()` 调用从 171 降为 0。网络文件对象仍有独立于 syscall
  dispatch 的 namespace 查询，本轮将该所有权留在 file/socket 边界，避免把 syscall
  capability 错误扩散到可移植网络层；其最终方向应像 Linux 一样由 socket 持有 netns。

确定性红测先把 `handle_syscall` 的类型约束为接收 `&UserTaskRef`，旧实现因仍接收
`&Thread` 稳定编译失败；迁移后同一约束通过。`qperf-metrics` 新增
`current_thread_handle_queries`，用于在相同启动和 workload 下直接比较 scheduler current
强句柄查询次数，不用从 QEMU 总时间反推。该计数只在 qperf feature 下启用，普通内核热路径
不增加原子操作。

#### current 强句柄与四架构 publication

显式传播消除了 syscall 子树的重复查询，但 Starry VM 指针、文件对象和内核辅助层仍需要在
无法携带 syscall borrow 的边界取得强句柄。旧 `current_thread_handle()` 为此进入 IRQ guard，
借用 `CpuLocal` 的可变 owner gate，再从 `dispatch.current_core` 克隆 `Arc`。这仍把 Linux 的
本地 `current` 与 runqueue owner 混在一起：一轮 x86 wakeup workload 中即使 syscall 已显式
传播任务，仍出现 1,900,741 次 owner-side handle 查询。

Linux v7.1 的共同模型不是“四架构分别维护一套 scheduler current”，而是架构寄存器或
per-CPU 固定槽选择当前 `task_struct`，`get_task_struct()` 只在需要跨当前执行区间持有任务时
增加引用。TGOSKits 四架构已经统一由 `TaskContext.task_local.current_header` 在裸切换尾部恢复
固定的 `CurrentThreadHeader`：x86 使用 GS CPU anchor，AArch64 使用 `sp_el0` 或 TLS anchor，
RISC-V 使用 `tp` 或 `sscratch` anchor，LoongArch 使用 `tp` 或 `r21` anchor。因此强句柄也应
从同一个 header 所属 runtime context 派生，而不是再访问 runqueue。

本轮把该边界收敛为 `CurrentThreadPublication { identity, owner }`：

- ax-task 在线程 ID 分配后、进入 runqueue 前一次性绑定 generation-bearing identity 与
  `ThreadCore` 的 opaque owner 地址；ax-runtime 把 publication 保存在 pinned
  `RuntimeContext`，header cookie 只指向这份不可变 publication；
- `current_thread_id()` 与 `current_thread_handle()` 读取同一个 publication。后者只持有一次
  preemption pin，在 `CpuLocal.current_core` 仍保留 owner-side `Arc` 的证明下增加 strong
  count，再通过 `ThreadHandle::from_core` 获取正常 external reaper lease；
- publication 的裸地址不能单独升级、不能跨 context switch 使用，也不能绕过
  `ThreadHandle` 的 external lease。退出线程仍由 scheduler handoff 保留到 switch tail 清除
  `on_cpu`；tail 前 reaper 不可能取得最后一个 strong owner；
- 运行时同时校验 publication identity 与克隆后 `ThreadHandle::id()`，错误或半绑定的
  publication 返回 `InvalidRuntimeHandle`，未绑定 bootstrap context 保持
  `NoRunnableThread`/`NotInitialized` 的原错误区分；
- 四架构汇编、`CurrentThreadHeader` 64B ABI 和 CPU-local register contract 不需要分叉，只有
  runtime cookie 的解释从 packed identity 升级为 typed publication 指针。

确定性红测在旧实现上观察到一次 `current_thread_handle()` 产生一个 CPU owner claim 且没有
migration pin；新实现要求 owner claim 为 0、remote endpoint 读取为 0、migration pin 恰为 1。
测试同时覆盖绑定 identity/owner 一致性、重复绑定、registry lock 持有期间的 current
extension 取得，以及原有 exit/switch-tail/reap 顺序。

Starry uaccess 也删除了同一次 copy 中的第二次任务查询：`prepare_user_memory()` 返回已经校验
并固定的 `UserTaskRef`，随后 fault scope 直接借用它；独立 page-fault trap 仍按自己的异常入口
解析 current。返回类型红测在旧 `VmResult<()>` 实现上稳定编译失败，新实现要求
`VmResult<UserTaskRef>`。

同一 x86_64 TCG、相同镜像和 workload 的前后结果如下（基线为 d69dba054）：

- `current_thread_handle_queries`：2,743,554 → 948,430（-65.4%）；
- 完整 QEMU：79.03s → 76.82s（-2.8%）；clock-pair：27.072 → 22.712 微秒（-16.1%）；
- OTHER p50：同核 123.643 → 110.935（-10.3%）、跨核 113.317 → 108.520（-4.2%）、
  跨进程 114.464 → 109.264（-4.5%）、timer 214.519 → 197.693 微秒（-7.8%）；
- FIFO p50：三个 futex 场景分别改善 4.7%、3.6%、4.6%；timer 215.717 → 217.898 微秒
  （+1.0%，保留为 TCG 波动/后续 timer 审计项，不能声称全部指标已对齐）。

该结果证明本地 current 不再争用 scheduler owner，且显式 capability 能降低实际端到端开销；
它仍不是“已达到 Linux RT 同一绝对水平”的证据。后续比较继续以 Linux RT 同机 workload、
调用链和 tail latency 为准，而不是用查询次数下降替代最终性能验收。

### Starry 用户内存的显式 current capability

syscall current capability 传播后，旧 `starry-vm` 仍通过全局 `VmImpl` provider 在任意
`VmPtr`、`VmBytes` 或驱动 `ioctl` 内重新取得当前任务。这不仅重复前一节已经删除的
current-handle 查询，还掩盖了更重要的生命周期差异：同步 syscall 借用当前任务、异步
worker 保存用户指针、访问另一个进程的地址空间，三者不能共用一个无类型的全局入口。

Linux v7.1 对应边界是：

- 普通 `copy_from_user()` / `copy_to_user()` 只访问调用者当前 `mm`，fault 和睡眠都发生在
  当前 syscall 的任务上下文；
- `access_process_vm()` 先取得目标 `mm` 的稳定引用，`process_vm_readv/writev` 则通过
  `pin_user_pages_remote()` 为这次同步远程拷贝固定目标页，完成后解除固定；两者都不是把
  “当前任务”隐藏成通用远程访问能力；
- usbfs submit 先把 OUT payload 复制进内核拥有的 buffer。硬件 completion callback 只发布
  状态并唤醒等待者，`processcompl()` 在 reap syscall 的任务上下文把 IN payload 和 URB 结果
  写回用户空间。只有显式注册的 DMA coherent mapping 例外地跨异步期保存用户映射，并用
  VMA/URB 使用计数约束释放；它不是普通裸用户指针的兜底。

Starry 当前按这条所有权线破坏性收敛，不保留旧 provider 兼容层：

- `VmIo` 只是一项由调用者提供的能力，所有 `vm_load`、`vm_read`、`vm_write`、字符串和
  iovec helper 都显式借用 `&UserTaskRef`；syscall dispatcher 持有的强引用覆盖整个同步
  调用和其中的 park/resume，不需要再次查询 scheduler current；
- task-bound `VmPtr`、`VmMutPtr`、`VmBytes`、`VmBytesMut` 和 `IoVectorBuf` 保存的是不可逃逸的
  task borrow，而不是可跨线程升级的裸 extension 指针。零长度只能表示“不执行拷贝”，
  不能被当作用户地址有效性证明；
- `FileLike::ioctl` 与 `DeviceOps::ioctl` 显式接收当前任务。没有 current capability 的通用
  VFS node 不尝试猜测任务或调用驱动用户内存接口；ION 析构改用专用内核态 release 操作，
  不再伪造一个指向内核栈的用户 ioctl 参数；
- USB async owner 只保存内核 buffer、typed status 和作为 completion cookie 的用户 URB 地址值。
  worker/IRQ 不保存 task handle、不把该地址转换为可解引用引用，也不执行 faultable copy；
  reap syscall 使用自己的 current capability 完成最终写回，和 Linux `processcompl()` 的阶段
  划分一致；
- `UserMemoryProvider` 的 raw faultable copy 仍只允许用于当前正在执行的 task。若未来支持
  ptrace/process-vm 式跨任务读写，必须新增以稳定 address-space 引用和页固定/直接页表访问为
  边界的 capability，不能把另一个 `UserTaskRef` 传给当前 fault handler 冒充远程访问。

剩余受限边界是第三方 eBPF auxiliary trait：它的 callback 签名没有 current-task 参数，三个
adapter 暂时只能在 callback 入口解析 `current_user_task()`。BPF syscall attr 的用户访问已经
显式接收 dispatcher capability；后续应升级外部 trait，使 helper borrow 随调用链传入，而不是
把这个受约束桥接扩散成新的全局 provider。

确定性红测 `vm_access_requires_an_explicit_provider` 在旧 API 上因缺少显式 provider 稳定
编译失败，迁移后同一类型约束通过。另一个行为回归覆盖 `T` 大于单次 buffer 容量时
`vm_load_until_nul` 仍必须前进，避免步长被整数除法截断为零而永久循环。本检查点的性能验收
继续使用上一节同一 qperf workload：必须同时报告 current-handle 查询、完整 QEMU 时间、
raw clock-pair 和八个 wakeup p50；在新数据取得前不得把接口收敛本身表述为性能提升，更不能
据此声明已经达到 Linux PREEMPT_RT 水平。

### 2026-08-05 显式 capability 检查点的 Linux RT 同源对照

本检查点重新从 `/home/zhourui/linux-src` 的 Linux v7.1
`8cd9520d35a6c38db6567e97dd93b1f11f185dc6` 构建独立 x86_64 内核和 initramfs，配置明确启用
`CONFIG_PREEMPT_RT=y`、`CONFIG_SMP=y`、`CONFIG_NR_CPUS=2`、
`CONFIG_HIGH_RES_TIMERS=y`、`CONFIG_FUTEX_PI=y` 和 `CONFIG_HZ_1000=y`。Linux `/init`
与 Starry app 均由当前 `main.c`、`handoff.c`、`timer.c`、`stats.c` 以 `-O2 -pthread -static`
构建；Linux 只额外定义 `BENCH_INIT` 负责挂载 procfs、运行同一 workload 并关机。两边均使用
Q35/TCG、`-cpu max`、2 vCPU、512 MiB，并同时发布 `clock_read=raw_syscall` 与正式通过标志。

Starry 完整 app 命令耗时 94.88 秒，其中 QEMU 阶段 77.69 秒；Linux 直接 bzImage/initramfs
启动到关机耗时 41.43 秒。二者启动链不同：Starry 经过 OVMF、NVMe/rootfs 和网络设备，Linux
使用 BIOS 直接启动最小 initramfs，因此总用时只用于复现记录，不能当作 scheduler 倍数。
同源 workload 的唤醒分位数可以比较，结果如下，延迟单位为微秒：

| 策略与场景 | Linux p50 / p99 | Starry p50 / p99 | p50 倍数 |
| --- | ---: | ---: | ---: |
| OTHER，同 CPU 线程 futex | 14.574 / 51.953 | 103.454 / 198.230 | 7.10x |
| OTHER，跨 CPU 线程 futex | 26.830 / 75.548 | 109.595 / 208.996 | 4.08x |
| OTHER，跨 CPU 进程 futex | 30.475 / 78.335 | 106.516 / 206.496 | 3.50x |
| OTHER，绝对 timer | 48.025 / 122.214 | 161.404 / 310.214 | 3.36x |
| FIFO:80，同 CPU 线程 futex | 12.452 / 49.819 | 104.214 / 201.023 | 8.37x |
| FIFO:80，跨 CPU 线程 futex | 23.382 / 65.785 | 104.398 / 202.592 | 4.46x |
| FIFO:80，跨 CPU 进程 futex | 25.452 / 67.536 | 108.159 / 215.144 | 4.25x |
| FIFO:80，绝对 timer | 33.744 / 100.079 | 165.470 / 317.074 | 4.90x |

Linux/Starry raw clock-pair 最小值分别为 0.665/19.962 微秒。这个测量成本包含在每个样本中，
但不能从跨线程分位数中直接相减。Starry FIFO 同核 futex 还出现两个超过 10 毫秒的样本，最大
50.405 毫秒；Linux 对应最大值为 0.249 毫秒。当前证据因此明确否定“已经达到 Linux RT
同一水平”。源码调用链审计把下一阶段优先级收敛为：合并 `clock_gettime` 的两次 faultable
用户写、缩短 wake/runqueue 事务、把 policy 更新改为 Linux rq-lock 式同步提交，并统一
scheduler-work generation 与 switch-tail 的完成协议。不能用提高 tick 频率或加入兼容轮询掩盖
这些所有权问题。

### 2026-08-05 cpupri HIGHER、可迁移 overload 与 running dispatch

对 root-domain priority index 的增量 review 逐项对照 Linux v7.1 后，确认三项可以在同一
runqueue owner 阶段直接修复：

1. Linux `cpupri` 有独立的 `CPUPRI_HIGHER=100`。旧实现只有 100 个桶，DL current 或
   queued 因没有 RT priority 被发布成 NORMAL，RT wake 会先投到不能抢占的 DL CPU，再经
   owner push 二次迁移。当前索引扩为 101 桶，只要 rq 存在 runnable DL entity 就发布
   HIGHER；最后一个 DL entity 离开后，在同一 rq summary 事务中恢复实际 RT priority。
2. Linux 的 pushable RT/DL entity 必须满足 `nr_cpus_allowed > 1`。旧 summary 只判断
   `workload > 1`，被单 CPU affinity 固定的候选也会发布 overload、发送无效 IPI。当前
   `QueuedThread` 保存随 affinity generation 更新的“可迁移能力”，增量 pushable cache
   只考虑能离开 owner rq 的实体；publication 仍为 O(1)，不为每次 wake 扫描 runnable set。
3. Linux 的 running entity 在 `update_curr()` 后仍是当前 rq entity，只有真实
   `put_prev_task()` 才离开 current。旧 safe point 无论是否切换都会 take dispatch、提交到
   thread state、重新构造并 install。当前无切换路径原地结算 `CurrentDispatch`，只增量同步
   task-context 可观察的 runtime/entity 状态；真实切换才结束 runtime interval、释放 DL donor
   baton 并进入 schedule-out/pick/set-next。这样既避免重复构造，也不把 CPU-local CBS 副本
   与线程可观察状态分成永久双真相。

三条原始行为红测分别稳定得到：RT 错留在 DL CPU、不可迁移 RT wake 发送 1 次 IPI、work-only
safe point 重建 1 次 dispatch。修复后同一断言为目标 CPU0、0 次 IPI、0 次重建；另有 affinity
窄化再放宽回归验证 cached pushability 随 owner-control generation 更新。第一次仅删除 dispatch
commit 会让 GRUB 测试观察到未同步的 CBS runtime，该失败未放宽；最终实现明确分离“运行态
增量同步”和“切出时最终提交”，`cargo test -p ax-task` 的 301 个 unit、21 个 loom 及全部
integration/doc test 通过。

review 中其余建议按所有权依赖处理：GRUB `extra_bw` 与跨 CPU runtime borrowing 必须先有
独立 root-domain bandwidth owner，不能只加一个永远为零的 per-rq 字段；blocked DL 在
zero-lag 前保留 `bandwidth_cpu` 与 Linux non-contending 语义一致，不是现有正确性缺陷。
RT/DL push 继续使用 generation-bearing owner doorbell，不改成 waker 同时持两个 rq lock；
Linux 的 `RT_PUSH_IPI` 同样通过 root-domain IRQ work 避免跨 rq 锁风暴。后续检查点已经补齐
`need_pull_rt_task()` 等价的 priority-drop 触发、`rto/dlo` overload 索引和单一 root-domain
push iterator；不能恢复旧 wake inbox、同步双 rq 或逐 overload CPU 广播的兼容路径。本节尚无
新的端到端性能数据，不据此声称已经达到 Linux PREEMPT_RT 水平。

### 2026-08-05 逻辑期限、物理 clockevent 与绝对睡眠

继续对照 Linux v7.1 的 `kernel/time/hrtimer.c::hrtimer_interrupt()`、
`hrtimer_nanosleep()` 和 `kernel/time/clockevents.c::clockevents_program_event()` 后，确认
旧实现把两个不同层次混在一起：ax-task 通过 `TaskRuntime::timer_resolution_ns()` 读取物理
设备粒度，并把 future task deadline 与 scheduler boundary 向后平移；与此同时，
`LocalClockEvent` 只在 selected minimum 变早时重写设备，变晚时保留已经失效的旧 arm，
等待一次多余 IRQ 再纠正。

当前实现破坏性删除 `TaskRuntime::timer_resolution_ns()` 及所有 resolution 参数。ax-task 的
deadline heap、CBS 和 rq boundary 只发布精确逻辑单调时钟值；过期值、sub-tick 值与 ns/tick
饱和转换只由四架构 clockevent backend 处理。`LocalClockEvent` 则把 selected minimum 作为
唯一物理真相：变早、变晚、删除来源都立即产生一个精确 `Program/Stop`，`Firing` 期间仍只在
finish 合并提交一次。deadline/deferred-work 语义没有变化时保留原 generation，避免每次
timer IRQ 制造无意义 publication 和硬件 reconciliation。

Starry `clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME)` 原先先用第一次时钟读数把绝对值
变成相对时长，随后 `sleep_impl()` 再用第二次 monotonic 读数加回；两次读数之间的处理成本
因此永久推迟用户期限。现在 `SleepDeadline` 显式携带 `Monotonic/Realtime` 时钟域：monotonic
绝对值原样进入 scheduler park，realtime 只在 user-wait 边界转换一次，relative sleep 统一
用 monotonic elapsed 计算 `rem`。这与 Linux 直接把 `timespec64_to_ktime()` 交给
`hrtimer_nanosleep(..., HRTIMER_MODE_ABS, clockid)` 的语义一致，不保留旧的 duration 兼容层。

三类最低层红测在旧实现中稳定失败：逻辑 deadline 2ns 被 10ns 设备粒度平移；task deadline
从 300ns 改到 400ns 时没有重编程物理 owner；两次 monotonic 读数相差 25ns 时，1,000ns 的
绝对睡眠被解析为 1,025ns。修复后对应精确值为 2ns、`Program(400ns)` 和 1,000ns；另有
重复 clockevent pass 断言未变事实保持同一 generation。`cargo test -p ax-task` 的 302 个
unit、21 个 loom、全部 integration/doc test，以及 `starry-kernel` 26 个 clippy 配置通过。
现有 x86_64 Starry timer-family 原始 syscall 组也保持 136/136、分组 3/3 正式通过，目标
QEMU 命令总耗时 50.15 秒；它覆盖相对/绝对 nanosleep、信号中断、剩余时间和 timerfd/POSIX
timer 组合，未以放宽 ABI 断言换取通过。

本轮 review 的 N1(cpupri HIGHER)、C5(不可迁移 overload) 与 running dispatch 已由上一检查点
完成。GRUB `extra_bw`、root-domain runtime borrowing 和 running 留队模型仍是下一阶段的调度
架构工作；它们不会通过在现有 per-rq 字段旁增加兼容镜像来实现，而会先建立唯一
root-domain bandwidth owner，再调整 rq class 生命周期。owner-side RT/DL push 保留
generation-bearing doorbell；后续检查点已按 Linux `RT_PUSH_IPI` 增加单一 root-domain
iterator，避免 wake 路径同步持有任意两个 rq 锁，也避免逐 overload CPU 广播，因此不采纳
“waker 无条件同步双 rq push”的建议。

### 2026-08-05 LoongArch IPI 提交语义

远端 CI 在 LoongArch ArceOS `task-ipi` callback burst 中超时，而当前 head 的单例通常通过。
对照 Linux v7.1 `arch/loongarch/kernel/smp.c::ipi_write_action()` 后确认，Linux 每次写
`IOCSR_IPI_SEND` 都设置 `IOCSR_IPI_SEND_BLOCKING`；该位只等待共享 send transport 接受命令，
不等待目标 CPU 执行 handler。somehal 原实现明确用非 blocking command，在多个 producer
快速写同一发送寄存器时可能丢掉软件队列发布后的最后一个物理边沿，因此恢复 IPI 能让队列
突然继续推进。

LoongArch command 编码现已拆为可在 host 运行的纯值模型。确定性旧红测精确断言 runtime
command 的 blocking 位原为 0，修复后为 1；目标 `task-ipi` 随后在 4 vCPU LoongArch QEMU
连续 10/10 正式通过。该修复不改变 scheduler doorbell 的 generation/claim/drain 协议，也
不把远程 wake 改成同步回调。后续阶段已经删除 `ax-ipi` boxed callback，并按下面的
hard-call/stopper 分层完成收口。

### 2026-08-05 hard-call 与 CPU stopper 分层

对照 Linux v7.1 `kernel/smp.c` 的 CSD、`include/linux/llist.h`、
`kernel/stop_machine.c` 和 `kernel/sched/stop_task.c` 后，跨 CPU 工作被拆成四条互不兼容的
所有权通道：

1. 平台 IPI 只传递物理边沿；
2. scheduler doorbell 只发布 generation-bearing 的 reschedule/task-work 状态；
3. `ax-ipi` hard-call 只接受调用方固定地址、同步等待的 raw operation；
4. 需要闭包、分配、等待或 OS 生命周期的工作进入 task-context worker。

`ax-ipi` 不再提供 `Callback`、`MulticastCallback`、`run_on_cpu()` 或
`run_on_each_cpu()`。同步请求是调用方栈上 pin 的 `HardCall`，只保存函数指针和借用参数；
发布后不得超时放弃。producer 使用 Release CAS 发布，只有空到非空的 producer 发送物理
IPI；目标 CPU 用原子 swap 整批摘链、反转为 FIFO，并最多执行 64 项。超出 budget 的
owner-only remainder 先于 IRQ 期间的新请求继续执行，并给本 CPU 重新触发 IPI。硬中断因此
不分配、不析构引用计数对象、不取得远端锁，也不存在逐节点 CAS pop 的 ABA 窗口。

Starry 内核文本更新使用一 CPU 一个的持久 stopper task。命令、完成等待和全局串行化发生在
任务上下文的 `PiMutex`/`WaitQueue`；所有远端 stopper 都进入 parked 状态后，才在
`NoPreemptIrqSave` 区间执行本 CPU 同步动作。stopper 使用独立 `KernelStop` 调度类：它高于
Deadline、POSIX RT、Fair 和 Idle，但不进入 RT priority array、cpupri/cpudl、RT/DL 带宽、
placement demand 或迁移扫描。每个 rq 只有一个 stopper 槽位，不能把 priority 100 当作
POSIX RT 兼容实现；Starry 调度 ABI 也不得暴露该内部策略。

当前平台 CPU 集在启动后保持固定，所以 stopper coordinator 以 IRQ framework 的 online
快照选择目标。若以后启用运行时 CPU hotplug，必须像 Linux `cpus_read_lock()` 一样先增加
独立 topology read-side lease：撤销 online 前禁用 stopper endpoint，并等待已提交命令、
hard-call 和 scheduler doorbell 全部 quiesce；不能只在 wait predicate 中重复读取布尔值。

### 2026-08-05 root-domain Deadline 带宽与 GRUB `extra_bw`

Linux v7.1 的 `struct root_domain` 同时拥有 online span、cpupri/cpudl 与 `dl_bw`；各 CPU
`dl_rq` 只拥有本地 `this_bw`、`running_bw`、`extra_bw` 和 `max_bw`。旧实现却把 Deadline
admission 放在通用线程 registry，把 online mask 放在另一把 root-domain 锁，再把
cpupri/cpudl 作为 `TaskSystem` 的第三个平级字段。GRUB 热路径只能看到本地
`this_bw - running_bw`，完全不知道 root domain 尚未预留的容量。

当前实现把这些事实收敛到唯一 `RootDomain`：

- topology 与 `DeadlineAdmission` 在同一 root-domain 状态事务中更新；
- cpupri/cpudl 继续由目标 rq 锁串行发布，并作为 root-domain 的派生索引存在。没有为每次
  wake 增加全局 root-domain 锁；同一 CPU 的两个 publisher 已由该 CPU 的 raw rq lock
  排除，不能把并不存在的双写竞态当作理由退化热路径；
- 每个稳定 `CpuRemote` 与它拥有的 rq 一一对应，并保存自己的原子
  `deadline_extra_bw_scaled`，即 Linux `dl_rq.extra_bw`。root-domain reservation 使用与 Linux
  `dl_bw.total_bw`、`dl_se.dl_bw` 相同的 `u64` 固定点类型；只有比例乘法的中间值使用
  `u128`，不存在写入线程状态时的截断或 `u64::MAX` 兜底；
- admission add/remove 按 Linux `__dl_add()`/`__dl_sub()` 对每个 reservation 先执行
  `reservation / nr_online`，再增减每个 online rq 的 `extra_bw`，不能先聚合 reservation
  再统一除法。策略替换使用一个 `replace(old, new)` 事务，等价于
  `total_bw - old + new`，并分别舍入 old/new，不能用 `(new - old) / nr_online` 替代；
- CPU topology 改变时等价执行 Linux `dl_clear_root_domain()` 与逐 task
  `dl_add_task_root_domain()`：registry 扫描每个 committed thread 的
  `max(active_reservation, desired_reservation)`，并包含 thread construction 尚未发布到
  registry record 的 slot-owned reservation；offline rq 重置为 `Umax`，online rq 从每个
  reservation 重新构建 `extra_bw`，不会从聚合 total 反推而丢失逐 reservation 舍入；
- 普通 runtime charge 在持有本 rq raw lock 时只 Acquire 读取本 rq 的 `extra_bw`，不进入
  root-domain 锁，也不存在一个可被所有 CPU 共同读取的全局 `extra_bw` 标量；
- reservation 新增、策略应用、退出和 registry removal 都由显式 root-domain 事务同步提交。
  registry 只返回待释放的精确值，无权直接修改 admission；旧
  `pending_deadline_releases`、读取侧补偿和 `RootDomainGuard::drop()` 隐式发布已经删除；
- CPU down 在关闭 placement 前执行 Linux `dl_bw_deactivate()` 等价容量检查。若
  `Ureserved > (nr_online - 1) * Umax`，直接返回 `DeadlineAdmission`，CPU lifecycle 与
  online mask 均保持原状；通过检查后，online mask、admission capacity 和所有 rq 的
  `extra_bw` 在同一 root-domain 事务中更新；
- runqueue 继续唯一拥有本地 `this_bw/running_bw`。GRUB 按 Linux 公式使用
  `max{u, Umax - Uinactive - Uextra}`，而不是从 root-domain 读取一个兼容镜像。

确定性红测使用单 CPU、`U=0.5` 的唯一 RECLAIM Deadline 任务。旧实现忽略
`Uextra=0.45`，100 ns 墙钟后预算从 500 降到 400；新实现与 Linux `grub_reclaim()` 一样
按固定点乘法向下截断，`floor(100 * 0.5 / 0.95)` 只扣 52，剩余 448。换算、带宽相减和
runtime charge 都以 admission/runqueue 不变量为前提；不再用 `unwrap_or(u64::MAX)`、
`saturating_sub()` 或向上取整把异常吞成保守计费。同一测试先稳定失败，再由上述 owner
重构转绿。后续复审又增加 CPU hotplug 红测：两 CPU 已预留 150% 带宽时，旧实现错误允许
下线一个 CPU；新实现按 Linux 在 topology mutation 前拒绝。另有确定性测试约束 75%
reservation 在 1 CPU、2 CPU、再次 1 CPU 时的 per-rq `extra_bw` 分别为 20%、57.5%、20%，
以及 detached policy release 在 API 返回前把本 rq `extra_bw` 从 45% 恢复到 95%。两个额外
红测使用每个 reservation 仅 1 个固定点单位的边界值：旧聚合除法在两 CPU 上错误扣除 1，
且旧差值式策略更新遗漏 `old/n` 与 `new/n` 的舍入差；新实现逐 reservation add/rebuild 和
原子 replace 后均与 Linux 一致。`DeadlineAdmission::release()` 另有 underflow 红测，旧
`saturating_sub()` 会返回成功，新实现明确返回 `InvalidConfiguration`，root-domain owner
把这种情况视为内部账本不变量破坏。

这一阶段补齐 A4 与 Deadline CPU-down admission。后续 owner-rq 事务已经实现 blocked
Deadline reservation 与 zero-lag/CBS registration 的源 rq detach、目标 rq attach；
`reservation_owner` 是带宽账本 owner，`SchedulerPlacement::control_owner()` 是物理 placement
owner，二者不再互作 fallback。running-in-rq 已由下一节完成。

### 2026-08-05 RT/DL running-in-rq 与独立 pushable 所有权

review 提出的 A5 方向有效，但不能把“running 留队”机械套到所有调度类。Linux v7.1 的
`set_next_entity()` 会把 Fair current 从 EEVDF 红黑树摘除，`put_prev_entity()` 再插回；
`set_next_task_rt()` 与 `set_next_task_dl()` 则保留 current 在各自 active array/tree 中，
只从 pushable 索引删除，`put_prev_task_*()` 再恢复 pushable。ax-task 现在按 class 保留这项
差异，不建立统一但错误的兼容模型。

重构后的 rq 所有权如下：

- RT/DL 的 generation-bearing intrusive node 在 dispatch 期间继续属于 active 结构；pick 只
  把它标记为 `linked_current`，不再归还 node storage、注销 membership、重新分配 sequence；
- `len`、placement demand 与 remote runnable summary 表示 queued work，不把 physically-linked
  current 重复计数；current 的优先级仍由同一 rq 锁内的 `CurrentClassState` 发布；
- migration eligibility 由独立、预分配的 generation-bearing `PushableIndex` membership
  拥有，只记录 slot generation、class count 与 RT quota-exempt count；候选顺序唯一归 DL tree、
  RT priority queue 和 Fair EEVDF tree，不再另建跨 class priority heap。`set_next` 只删除
  pushable membership，`put_prev` 只恢复 membership；active ownership 与可迁移性不再共用；
- Deadline current 的 CBS accounting 在一个 dispatch interval 内保持 queue key 稳定，只在
  rq 锁内替换 active node 的 augmented payload，不执行 remove/insert；absolute deadline
  的变化由 `put_prev` 在 current 离开 dispatch 时统一 remove/reinsert 并 rekey；
- RR yield 与 quantum expiry 都只移动原 intrusive node，但二者不共享 quantum 生命周期：
  `yield_task_rt()` 只移到同优先级尾部并保留剩余 quantum；`task_tick_rt()` 在耗尽时立即刷新
  quantum，且只有同优先级存在 peer 才移到队尾并请求 reschedule。普通 FIFO/未耗尽 RR
  preemption 保持原位置；Fair 继续沿用原 EEVDF remove/insert 语义。

两项最低层红测在旧实现稳定证明 RT 与 Deadline pick 后 membership 已消失；新实现要求
active membership 保留、queued count 为零、pushable membership 不包含 current。确定性 reference model 也改为
Linux 语义：未耗尽 RT/DL 的 scheduler entry 不产生新 arrival sequence，耗尽 CBS 才退出 active
集合。不是放宽输出断言。当前检查点执行 `cargo test -p ax-task --quiet`，313 个 unit、
21 个 loom 以及全部 integration/doctest 通过。后续仍需拆分 `CurrentDispatch`
快照/记账；task 级 `SwitchingOut/ExitedAwaitingTail` 已删除，不能重新引入与 per-CPU
`SwitchHandoff` 并行的兼容状态。

### 2026-08-05 Linux `task_cpu/on_rq/on_cpu` 与 `TASK_WAKING`

placement 现在直接采用 Linux v7.1 的三组正交事实，而不是一个可被 hint 覆盖的综合状态：

- `task_cpu` 是最后提交的 rq assignment；`on_rq` 只有 `None / Queued / Migrating`；
- `on_cpu` 是物理执行 claim，只允许 switch tail 的 `finish_task()` 清除；
- `migration_request` 只是尚未提交的 affinity 目标，不能重定向已经携带目标 rq publication
  lease 与 thread inbox reservation 的 `PreparedMigrationDelivery`；
- `wake_cpu_hint` 只参与 `select_task_rq` 等价决策，不属于 placement，也不通过
  `ThreadHandle::assigned_cpu()` 暴露。

这消除了两个旧问题。其一，新的 affinity request 不能在 carrier 已经离开源 rq 后偷偷改写
目标；exit 只能显式取消尚未消费的 remote handoff，目标仍负责 drain carrier 并释放 publication
lease。其二，blocked task 可能在 outgoing context 尚未完成 switch tail 时被唤醒。waker 在
唯一 task sched lock 下取得稳定 target publication、提交 `Waking`，再以 Acquire 等待
`finish_task()` Release 清除 `on_cpu`，随后由同一个 waker 直接锁目标 rq 并提交
`Waking -> Ready`。switch tail 在 release `on_cpu` 前不取得 task lock，也不替 waker
完成 wake/enqueue；release 后的 exit/affinity 元数据收尾不能反向阻塞 waker。这对应 Linux
PREEMPT_RT 关闭 `TTWU_QUEUE` 后由 `try_to_wake_up()` 持有完整唤醒事务的所有权。

Idle 也按 Linux 特殊调度类处理：运行中的 idle 始终保持 `on_rq=Queued`，schedule-out 只执行
`put_prev`，不会借普通 block 路径把它变成 detached。确定性回归分别覆盖 wake-before-tail、
后续 affinity request 不改写 committed target、exit 取消未消费 carrier、连续 idle 选择和
blocked task 在 CPU offline 后保留旧 `task_cpu` 但更新 wake hint。普通 switch-tail 的最低层
红测还要求完成阶段不再打开任何 `OwnerRqTxn`；wake/park 竞态测试要求 waker 在旧 stack
仍为 `on_cpu` 时保持 `Waking` 且不返回，release 后由该 waker 完成 enqueue。

检查点验证还包括 `cargo xtask clippy --package ax-task` 的 base/qperf 两项，以及
`cargo xtask clippy --package starry-kernel` 的 26 项 feature/configuration matrix，全部通过。
本检查点没有启动 QEMU，不能把编译与最低层行为测试描述为四架构运行结果。

### 2026-08-05 root-domain overload 与串行 push iterator

再次完整对照 Linux v7.1 `kernel/sched/rt.c::rt_set_overload()`、
`rt_clear_overload()`、`need_pull_rt_task()`、`tell_cpu_to_push()`、
`rto_push_irq_work_func()`，以及 `kernel/sched/deadline.c::dl_set_overload()`、
`dl_clear_overload()`、`deadline_queue_push_tasks()` 后，确认上一实现虽然已经有 cpupri/cpudl，
但仍缺少 Linux 决定“哪些源 rq 需要 push”的另一组 root-domain 权威事实。旧代码从
`CpuLoadSummary::{workload, pushable_class}` 重新推导 overload；当目标 CPU 从高优先级 RT/DL
任务切换到较低优先级任务时，也没有 `need_pull_*` 等价触发。结果是源 rq 上已经排队的
RT/DL 任务只能等待下一次偶然 enqueue、tick 或 idle pull。

当前实现直接采用 Linux 的所有权划分：

- 每个 rq 的 `PushableIndex` 在同一 rq 锁事务内维护 Deadline、RT class membership count；
  running entity 不在 pushable index，单 CPU affinity entity 也不会进入。root-domain 由此发布
  独立的 `dlo_mask/dlo_count` 与 `rto_mask/rto_count`，不再从 load summary 建立第二真相；
- set 路径先 Release 发布 mask bit，再发布 count；clear 路径先减少 count，再清 bit。读取者只有
  观察到非零 count 才扫描 mask，对应 Linux `smp_wmb()` 与 pull-side `smp_rmb()` 的可见性契约；
- owner selection 同时保存 previous/next 的 `SchedulingUrgency`，只比较 class 与 class-local
  priority/deadline，不把 ThreadId、arrival sequence 等队列 tie-break 误判成优先级下降；
- 真正发生 RT/DL priority drop 时只增加 root-domain `requested_generation`。一个
  RT 和 DL 各有一个 `Idle / Published(cpu) / Claimed(cpu)` iterator，分别保存
  `scan_generation` 与 cursor；每一类同一时刻只给一个对应 overload owner 发布 scheduler
  doorbell；owner claim 后在自己的 rq/safe point 内只 push 同类候选，
  完成再交给下一 owner。扫描期间的新请求只推进 generation，本轮结束后重扫，等价于 Linux
  `rto_loop_next/rto_loop/rto_cpu`，不会形成逐 CPU IPI storm；
- ax-task 的迁移事务要求源 rq owner 独占 intrusive node 与 publication lease。RT 与 DL 共享
  迁移 carrier 协议，但不共享 overload iterator 或候选选择；它们分别对应 Linux `rto_mask`
  与 `dlo_mask`，没有退回 target CPU 同步锁多个 rq 的第二迁移协议；
- 每个 safe point 最多提交一次跨 rq 迁移。若迁移成功且源仍 overload，iterator 保留该 source
  并重新发布本地 owner work；若没有可迁移目标则交给下一 source。这样保持 owner callback 有界，
  同时具备 Linux `push_rt_tasks()/push_dl_tasks()` 的“只要持续取得进展就继续”语义。

审计还发现 scheduler doorbell 原先只设置通用 work bit；若当前任务无需被抢占，
`schedule_if_requested()` 会直接返回，根本不执行 rq balance callback。新实现把 callback service
放入无切换 safe point：先同步 running dispatch 和 root-domain publication，再 claim iterator，
最后决定是否需要下一 generation。调度选择和资源 switch tail 仍不因 balance callback 被伪造为
一次上下文切换。

两个确定性红测分别固定旧失败：CPU1 从 RT50 降到 Fair 后，CPU0 的 RT10 永远没有 owner
doorbell；两个 overload source 同时存在时，旧 fan-out 一次发送 2 个 IPI。新实现前一测先观察
到 1 个 serialized owner edge，并在 CPU0 无切换 safe point 提交到 CPU1 的 migration；后一测
首次只发送 1 个 IPI，CPU0 完成后才把下一 generation 交给 CPU2。该检查点尚未重跑 x86_64
完整 Starry QEMU 或 Linux RT wake-latency 对比，不能据最低层红绿测试声称端到端性能已经对齐。
当前 `cargo test -p ax-task --quiet` 的 336 个 unit、21 个 loom 及全部 integration/doc test，
`cargo xtask clippy --package ax-task` 的 2 项和 `cargo xtask clippy --package ax-runtime` 的
27 项 feature/configuration matrix 均通过。

### 2026-08-06 PI 解锁与 wake_q 单一事务

再次逐行核对 Linux v7.1 `kernel/locking/rtmutex.c::mark_wakeup_next_waiter()`、
`rt_mutex_slowunlock()`、`rt_mutex_wake_up_q()` 和
`include/linux/sched/wake_q.h` 后，确认旧 facade 把一个 Linux 内部不可分割的解锁事务拆成了
两个可独立调用的公开步骤。`TaskSystem::pi_mutex_release()` 只提交 waiter 选择、旧 owner deboost
与 ownerless publication，然后把 `ThreadWakeHandle` 交给 ax-sync；ax-sync 必须另外调用
`pi_wake()`。这个 handle 可以被正常 drop，类型系统允许已经提交 handoff 的 waiter永久停留在
Blocked 状态。调用方额外建立的 `NoPreempt` guard 还把核心真正依赖的调度约束泄漏到了锁实现层，
形成了两套 preemption 生命周期。

Linux 的主路径没有这个可选边界：`rtmutex->wait_lock` 与 owner `pi_lock` 内选择 top waiter、
完成 deboost、把锁发布为 ownerless/has-waiters 后，`mark_wakeup_next_waiter()` 在仍禁止抢占时把
任务放入 task-embedded `wake_q`；随后释放元数据锁，由 rtmutex 核心执行 `wake_up_q()`，最后才
恢复抢占。wake_q 只把实际 rq 激活延迟到锁外，不把“是否 wake”授权给外层调用方。

当前实现据此直接删除旧分层，不保留兼容入口或失败兜底：

- 新的 move-only `PreemptScope` 是 ax-task 内部事务 guard，覆盖旧 owner deboost、元数据锁释放、
  `wake_thread_direct()` publication，直到 release 返回；`PreemptTicketLock` 复用同一 guard，
  facade 不再维护第二个 `RuntimePreemptGuard`；
- `TaskSystem::pi_mutex_release()` 和 facade `pi_mutex_release_owned()` 只返回 `Result<()>`，
  ax-sync 不再接触 wake handle，也没有 `pi_wake()` 可调用或遗漏；
- waiter 选择、owner donation tree 更新和 ownerless publication 仍在 PI metadata 临界区内，
  rq wake 严格发生在所有 metadata guard drop 之后；
- handoff publication 一旦提交，selected waiter 变为 exited 或在 registry 中 unavailable 都是
  ax-task 内部不变量破坏。Linux 的 waiter tree 持有稳定 task 引用，退出前必须撤销等待关系；这里
  同样不能回滚已经完成的 owner/deboost 状态，也不能虚构第二条重试路径，因此直接触发 fatal
  invariant，并记录 selected waiter 的 generation-bearing identity；
- 原测试中 offline CPU 上的 Ready waiter和 `New` waiter都不是合法 release 前置状态。测试 fixture
  改为先上线 CPU 并建立可调度 Ready waiter，而不是在生产路径增加 Linux 中不存在的 offline/New
  fallback。

确定性回归 `pi_release_wakes_the_selected_waiter_before_returning` 在旧实现上稳定观察到 release
返回后 waiter 仍为 Blocked；新实现要求同一次 release 返回时 waiter 已经 Ready，从接口层证明 wake
不能被调用方遗漏。本检查点的 ax-task 337 个 unit、21 个 loom 及全部 integration/doc test、
ax-sync 全部 unit test 已通过；`cargo xtask clippy --package ax-task` 2 项、
`--package ax-sync` 3 项和 `--package ax-runtime` 27 项 feature/configuration matrix 全部通过。
这一步消除了重复的 facade/preempt 进入，但尚未重新测量 x86_64 Starry 完整耗时或 Linux RT
wake-latency，不能据结构收敛声称端到端性能变化。

### 2026-08-06 四架构 secondary boot 同步所有权

x86_64 ArceOS CI 曾在 CPU1 完成运行时初始化后，以
`timeout waiting APIC ID 0x2 online` 偶发终止。相同提交前后两次 CI 和本地四核完整测试都能
启动 CPU1/2/3，说明这不是固定 APIC 映射错误。旧 someboot 把架构 wake transport、AP 进入
Rust 和运行时 online 三个阶段压进 x86 私有的全局 `AP_BOOTED_ID`；BSP 只给 QEMU 500 ms，
而报错把 `_secondary_entry` 之前的超时错误描述成 scheduler online 失败。AArch64、RISC-V、
LoongArch 的固件或 IPI `cpu_on` 又只确认请求已接受，四架构因此没有共同的 AP 启动完成语义。

修改前完整核对 Linux v7.1 `kernel/cpu.c::cpuhp_can_boot_ap()`、
`cpuhp_ap_sync_alive()`、`cpuhp_bp_sync_alive()` 和
`arch/x86/kernel/smpboot.c::native_kick_ap()/do_boot_cpu()/start_secondary()`：

- 架构层只拥有 PSCI/SBI/mailbox 或 INIT/SIPI 的 wake transport；
- 通用 CPUHP 为每个 CPU 独立拥有 `DEAD / KICKED / ALIVE / SHOULD_ONLINE` 同步状态；
- AP 以 full-barrier 原子交换发布 `ALIVE`，BSP 只允许把匹配 CPU 的 `ALIVE` 原子推进为
  `SHOULD_ONLINE`，未知 limbo 状态不得猜测或重试；
- x86 当前 SIPI 使用 `APIC_DM_STARTUP == 0x600`，不携带 INIT 的 level-assert 位；
- BP 的 alive 等待上限是 10 秒，超时保留原状态并返回错误，不把延迟启动误当成功，也不创建
  另一条启动路径。

someboot 现在采用同一所有权模型：`PerCpuMeta` 继续只保存稳定的 trampoline ABI；每个动态
CPU area 另有一个 shutdown-lifetime `CpuBootSync`，避免原子状态与 metadata 的 Copy/cache
生命周期混合。通用 `power::cpu_on()` 在全局 CPU boot 写事务内发布 `KICKED`，调用更名后的
`ArchTrait::kick_secondary_cpu()`，等待目标 `ALIVE` 后发布 `SHOULD_ONLINE`。四架构都在完成各自
页表、最终栈并进入共享 `entry::secondary_entry()` 后报告 alive，再进入 OS secondary entry。
x86 私有 `AP_BOOTED_ID`、私有启动锁和 500 ms 轮询全部删除；INIT/SIPI 只负责投递，SIPI 编码
直接使用 Linux 的 `0x600`。运行时 scheduler online、IRQ ready 和 `INITED_CPUS` 仍由
ax-runtime 分阶段发布，不能被平台 alive 状态替代。

确定性红测 `startup_ipi_matches_linux_edge_triggered_delivery_mode` 在旧实现上得到
`0x4600 != 0x600`；新的 per-CPU 行为测试还固定了错误 CPU 的 alive 不能释放目标 CPU，以及
已经进入 `SHOULD_ONLINE` 的 limbo 状态不能重新 kick。该模型没有增加超时重试、APIC fallback
或旧全局确认值兼容路径。

当前检查点已通过 `cargo test -p someboot`（52 个单元测试及全部 integration test）、
`cargo xtask clippy --package someboot`（8 组配置）和
`cargo xtask clippy --package ax-runtime`（27 组配置）。四架构 ArceOS `rust/all` 与 C 套件均
在四核 QEMU 上通过；LoongArch 额外的单核 unaligned-fixup 也通过。每个架构的 AP 都在共享
同步点之后进入 ax-runtime，并完成 CPU1/2/3 的运行时初始化，因而验证覆盖的不只是固件接受
启动请求，还包括最终栈、页表、CPU-local 绑定和 OS secondary entry。

### 2026-08-06 单 CPU affinity 的调度类选核边界

FIFO 同 CPU futex 的精确 qperf 窗口显示，旧实现的
`RtCpuPriorityIndex::find_lower()` 占 99/1966 个 workload 样本（5.04%）。该场景的 sender 与
receiver 都固定在 CPU0；唤醒时 CPU0 正运行相同优先级的 FIFO task，旧代码仍从 normal 桶开始
扫描到 FIFO:80，最终才通过 preferred CPU 回退到唯一允许的 CPU。cpupri 的位图结构本身没有错，
错误在于调度类入口没有先应用 affinity cardinality 这一更强的约束。

修改前重新核对 Linux v7.1 `kernel/sched/rt.c::select_task_rq_rt()`、
`find_lowest_rq()`、`kernel/sched/deadline.c::find_later_rq()` 与
`include/linux/sched.h::task_struct`：RT 和 DL 都在 `nr_cpus_allowed == 1` 时完全跳过
cpupri/cpudl，`task_struct` 还独立维护 `nr_cpus_allowed`，不在每次 wake 时遍历 cpumask。这个
短路不是失败后的 fallback，而是“只有一个合法 owner 时，优先级索引不可能产生另一个目标”的
调度类前置条件。

当前 `CpuSet` 因此与 Linux 同样维护 authoritative `allowed_count`，所有 insert、remove 和
copy 事务同步更新 mask 与 cardinality；`is_migration_capable()` 不再重复扫描整个 topology。
`select_priority_cpu()` 在 RT、RR、DL 共用入口先解析唯一 allowed CPU，并只校验 online/excluded
条件；只有 affinity 允许迁移时才读取 root-domain cpupri/cpudl。没有增加缓存、超时、重试或
第二选核路径。

确定性回归 `singleton_rt_wake_bypasses_root_domain_priority_indexes` 在旧实现上稳定得到 lookup
次数 1，新实现为 0。相同 2 vCPU、Q35/TCG、leaf-callchain qperf 复测中，`find_lower` 降为
0/2028 个样本。两次 workload 窗口为 19.93 秒和 20.57 秒，约 3% 的单轮波动不足以形成整体
延迟改善结论；该阶段只确认错误扫描已经从正式 FIFO 同核调用链消失，端到端 p50 继续用无
采样完整 benchmark 验收。无采样复测的 raw clock-pair 为 17.762 微秒，FIFO 同核 p50 为
122.770 微秒，QEMU workload 为 91.20 秒；上一检查点对应值为 17.708 微秒、117.069 微秒和
89.05 秒。其余场景有升有降，当前单轮结果不支持整体性能改善声明，也不回退已经由 Linux
语义和确定性红绿测试证明正确的 affinity 边界；后续继续从新的 PI handoff、guard 和 current
identity 热点收敛固定成本。

### 2026-08-06 rq、选核与 runtime 能力边界最终收敛

最后一轮逐调用链复审删除了三类会让后续修错重新产生双轨的结构：

- `CpuLoadSummary` 只发布 Fair/sched-domain 所需的 demand 与 `fair_pushable`。RT/DL 的
  current priority、pushable priority 和 overloaded 不再出现在 load snapshot；唯一远程
  authority 是 rq 事务发布的 cpupri/cpudl 与 `rto/dlo` mask/count。RT/DL push 和 idle pull
  直接遍历 class mask，balance request 显式携带 class；失败后的 visited CPU set 使下一次
  owner pass 前进到下一 source，不自旋重试同一 rq。Fair snapshot 的 seqlock reader 等待
  owner 在不可睡眠的 rq publication 临界区写完匹配的偶数 generation；删除固定八次读取后
  返回 `None` 的自创恢复，否则 writer timing 会静默把合法 CPU 从 placement/balance 中删除；
- wake、首次 Ready、普通 affinity、current affinity、owner reconciliation 与 switch-out
  forced migration 都先读取有效 class/entity：RT 使用 `find_lowest_rq()` 等价 cpupri，DL
  使用 `find_later_rq()` 等价 cpudl，Fair 才读取 demand。只有 Linux `select_fallback_rq()`
  对应的拓扑恢复路径可以选择第一个 allowed/active CPU，不得用 queued-count 代替优先级；
- 旧 `PushableIndex` 跨 class 二叉堆没有生产消费者，而且复制了 DL tree、RT priority array
  与 Fair EEVDF tree 的排序，因此已整体删除。DL 自己维护 generation-bearing pushable
  索引，RT 自己维护按优先级位图；Fair 不伪装成 Linux `pushable_tasks`，只由 EEVDF tree
  保存迁移资格并向 sched-domain 发布派生的 `fair_pushable`。三个 class queue 各自是候选
  顺序的唯一 owner；
- `DeadlineBandwidthState::reservation_owner` 只表示 `this_bw/running_bw/zero-lag` 账本所在
  rq；`SchedulerPlacement::control_owner()` 只表示 queued/running/switch-tail 的物理 owner。
  policy replacement 可显式选择 reservation owner 执行带宽事务，但 affinity 和 wake 禁止
  把 reservation owner 当作 placement fallback；
- Linux v7.1 的 root `sched_rt_period_timer()` 明确使用 `HRTIMER_MODE_REL_HARD`，并在
  `do_sched_rt_period_timer()` 中扫描 online span。ax-task 因而保留同一 hard scheduler timer：
  每个 rq 先以 `rt_time==0 && !rt_nr_running && !rt_throttled` 快速跳过，必要时才取得 rq 与
  嵌套 runtime lock、补充 quota 并发布 reschedule；不能误改成通用 task soft timer。
  sleep/park/wait timeout 仍只由 soft-timer worker 唤醒。`rt_time/rt_runtime` 借贷账本继续使用
  独立 raw `rt_runtime_lock`，与 Linux `rt_rq->rt_runtime_lock` 及 root
  `rt_bandwidth.rt_runtime_lock` 的 runtime sharing/period 扫描锁域一致；rq eligibility 则只由
  owner rq 事务中的 `rt_throttled` 发布，二者不是同一事实的两份副本；
- ax-runtime 的 current-thread 读取不再把寄存器/anchor mismatch 转成空指针或 NONE。
  NONE 只表示永久 early bootstrap header 尚未绑定 scheduler cookie；已绑定 runtime context
  的 cookie/publication 不一致是致命架构不变量。IRQ guard 与 scheduler-tail 查询失败同样
  fatal，不得静默当作 `need_resched=false`。

`TaskRuntime` 保留一张 trait-FFI capability 表，因为它是 ax-task 到 OS 的链接边界，不保存
调度状态；文件按资源/context ABI、clock ABI、provider interface 拆分。`CpuLocal` 则按
dispatch/switch handoff、scheduler deadline/soft timer、idle polling facade 拆分。拆分没有
引入 supertrait、兼容转发或第二 owner。

### 2026-08-06 PREEMPT_RT PI 有效实体与 Deadline CBS 所有权

修改前重新核对 Linux v7.1 `init/Kconfig::SCHED_PROXY_EXEC`、
`kernel/sched/core.c::rt_mutex_setprio()`、`kernel/sched/deadline.c::pi_of()`、
`replenish_dl_entity()` 和 `update_curr_dl()`。`SCHED_PROXY_EXEC` 明确依赖
`!PREEMPT_RT`，因此 RT 配置不能采用 `rq->donor` 的 proxy-execution 记账，也不能把 donor
的可变 CBS runtime 当作 mutex owner 的执行预算。PREEMPT_RT 的实际模型是：

- FIFO/RR 的 `policy` 与嵌入 task 的 RT entity 不因 PI 改变；`rt_mutex_setprio()` 只改变
  effective priority。RR 的剩余 quantum 在 boost、抢占和 deboost 之间连续累计，FIFO 始终
  不产生 timeslice；
- 每个 task 都嵌入自己的 `sched_dl_entity` 可变执行账本。`p->dl.pi_se` 只把
  `dl_runtime/dl_period/dl_deadline` 参数解析到 donor；runtime、absolute deadline、throttled、
  overrun 和 timer lifetime 仍属于 owner 本地 entity；
- mutex owner 运行时只扣一次自己的本地 runtime，donor runtime 不变。boosted entity 预算
  耗尽后走 `ENQUEUE_REPLENISH` 等价路径，用 donor 参数立即补充 owner 本地账本并更新 EDF key，
  不等待普通 Deadline timer，也不把 overrun callback 重定向到 donor；
- deboost 只清除 effective donor/`pi_se`，恢复同一个 base entity，不能从 task-side overlay
  复制一份旧 accounting 覆盖 rq 已经累计的 runtime。

ax-task 因此把 policy state 收敛为一个可移动的 `ActiveSchedulingState`：base entity、可选的
cross-class inherited entity 与 effective policy 始终作为一个值在 task control 和 owner rq
之间转移，任一时刻只有一个物理 owner。RT 同类 PI 直接复用 base entity；Deadline inherited
entity 由 owner-local `DeadlineServer` 和 donor-parameter `DeadlineServer` 组成。旧
`pi_overlay/pi_saved_active`、task/rq 双份 active state、donor 双重扣费和 donor overrun 路径均已
删除。这个边界是 PREEMPT_RT 的正式实现，不得在后续适配中恢复 proxy accounting 或兼容镜像。

### 2026-08-06 最终反向审计与架构冻结

在进入统一修错前，按 Linux v7.1 `__schedule()`、`enqueue/dequeue_task_{rt,dl}()`、
`sched_rt_period_timer()`、`start_dl_timer()`、`update_rq_clock_task()`、`hrtimer_interrupt()`、
`irq_work_claim()` 和 `finish_task_switch()` 再次反向核验完整调用链。当前架构冻结为：

- `RunQueue::current` 是唯一 `rq->curr`。RT/DL current 保留在 active list/tree，Fair/stop
  current 离开 class tree 但仍计入 `nr_running`，dedicated idle 永不进入 Fair class 或
  `nr_running`。旧 `CpuRunQueueState::current`、`linked_current` 字段和独立 `nr_queued`
  计数均已删除；`linked_current`/`nr_queued` 只能由 `rq->curr` 与 `nr_running` 派生；
- class 生命周期只经过 `enqueue/dequeue/check_preempt/put_prev/pick/set_next/task_tick`。
  common rq 统一提交 current、membership、placement、runtime accounting 和 root-domain
  publication；class 后端不得再发布另一份 running/current 状态；
- `task_tick` 接受当前 rq、current identity 与 policy，不再把所有 class 压成脱离队列的
  `slice_expired -> resched` 布尔映射。RR quantum 的刷新和 active queue 轮转在同一 rq
  transaction 内完成；单一 RR 任务只刷新 quantum，FIFO 永不因 slice 触发调度，主动
  yield 不刷新 RR quantum；
- Deadline active tree、throttled membership、timer lifetime anchor、`this_bw/running_bw`
  账本和 class-local pushable index 全部位于同一个 `DeadlineRunQueue`。root-domain
  `total_bw/admission`、每 rq `this_bw/running_bw`、每 rq `extra_bw` 仍是 Linux 定义的三个
  不同语义，不能误合并为一项；拓扑/admission 写事务统一更新 `extra_bw` 镜像；
- task 的 base/effective scheduling state 作为一个值在 task control 与 rq 之间移动。FIFO/RR
  PI 只改 effective priority 并保留原 entity；Deadline PI 只借用 donor 参数，owner-local CBS
  账本单次扣费并在 boosted exhaustion 时立即 replenish。不存在 task-side overlay、donor
  runtime 扣费或第二份 rq accounting；
- RT bandwidth 保留 Linux 的两级 owner：root `rt_bandwidth` 独占 period hard timer 与
  runtime-sharing 总锁，每个 `rt_rq` 的嵌套 bandwidth lock 独占 `rt_time/rt_runtime`，owner
  rq 主锁独占 `rt_throttled` eligibility。hotplug loan reclaim、period scan 和 strict
  `rt_time > rt_runtime` 只在 owner rq 事务中把 ledger decision 转换为 throttle transition，
  不存在 task-owned quota 副本、原子 throttle 镜像或周期轮询恢复；
- scheduler request 的 generation word 是逻辑 request/ack 唯一 authority；runtime IPI
  doorbell 只运输该 generation 的物理边。task-work 的 work queues 是 payload authority，
  doorbell epoch 只表示 worker 尚未承认的通知，`IrqWaitCell` 只负责可合并的物理 wake；
  三者不复制 payload 或完成状态；
- `LocalClockEvent` 是物理 oneshot 唯一 owner，只有 `Offline/Idle/Armed/Firing`。scheduler
  deadline、periodic tick 与 task soft deadline 只作为逻辑 source 输入最近期限选择；hard
  scheduler timer 的 IRQ budget 耗尽后由 sticky scheduler request 在 owner safe point 继续，
  task timeout 的 remainder 则只发布给 `ktimers/<cpu>` worker。两条路径都不重新 arm 已过期
  边，不提供 `claim_due/recover_overdue` 或偶然 tick 扫描；
- membarrier 注册状态只属于共享 `mm` runtime state，使用 requested/ready 两阶段发布；
  `rq->membarrier_state` 只缓存 `rq->curr` 对应的同一状态。注册先发布 requested，再通过同步
  IPI 在目标 CPU 的 rq 锁内刷新并执行全屏障，最后发布 ready；expedited 命令从 rq current
  选择目标并等待固定 hard-call 完成。Starry process policy 不保存第二份注册状态，
  `CLONE_VM` 共享同一 `mm`，fork/exec 获得新 identity；
- runtime 当前 CPU 的 local/remote handle、current-thread publication、rq clock sample 和
  hardirq 累计值都来自对应 CPU 的已初始化 per-CPU/cpu-local owner。只有显式的 early
  bootstrap/query API 可以返回 `NONE/NotInitialized`；online scheduler fast path 的缺失、
  CPU 错配或 clock source 错配均为致命生命周期违例。

这次冻结只表明状态所有权和接口边界已统一，不代表测试、QEMU、CI 或性能已经通过。后续失败
先判断是否违反上述不变量；若不违反，才作为统一适配/实现错误修复，禁止恢复已删除字段、旧
API、超时重试、轮询推进或 silent fallback。

### 2026-08-07 所有权模型收口与调用方冻结

在统一编译前再次按 Linux v7.1 `rq`、`rt_bandwidth`、`rt_mutex`、`hrtimer` 和
`irq_work` 的生命周期反向扫描 ax-task、ax-runtime、Starry 与 AxVM。该轮不再增加兼容层，
而是把剩余实现直接收敛到已冻结的 owner 模型：

- `RunQueue` 的 accounting、balance、dispatch、lifecycle 与 membership 分模块保存各自不变量；
  `TaskSystem` 的 dispatch 分为 wake、bandwidth、current 与 policy，PI 分为 schedule、graph 与
  operations。拆分只缩小 owner transaction 的可见面，不复制 rq、placement 或 PI 状态；
- `ThreadCore` 的 lifecycle、policy、runtime accounting 与 wake state 分模块，ax-runtime 的
  thread publication、extension 与 lifecycle 分模块。强句柄仍只租用同一个 generation-bearing
  registry identity，不增加旧 `TaskInner`、`CurrentTask` 或 task-side scheduler mirror；
- coroutine、runqueue membership、idle-pull、root-domain push iterator、wait notification 和
  AxVM timer token 的 identity 空间耗尽都作为致命不变量处理，禁止整数回绕后复用旧 identity；
- root RT period timer 在 `Idle -> Armed` 时获得唯一 generation。重复 activate、owner CPU
  migration 和 `Firing` 期间的新 activity 都属于同一个已 armed timer 生命周期；finish 只消费
  move-only firing identity，不以重新编号掩盖并发 activation；
- PI lock 的销毁边界是 waiter/donation graph 已 quiesce。无 waiter 时，嵌入锁的 owner word
  不是调度器可观察的悬空边；因此不得自创“Drop 时必须 unlocked”规则。注册、handoff、deboost
  和 wake 仍在同一 PI 元数据事务中完成，锁外才执行 wake；
- Starry 的 `UserTaskRef` 是 scheduler handle 到 Linux thread/process identity 的唯一适配层，
  futex、signal interruption、timer worker 和 IRQ waiter 只消费 runtime facade，不恢复旧 ax-task
  对象；进程 identity 仍由 dev 的 `ProcessIdentity` 状态机独占；
- AxVM 的物理 IRQ endpoint 独占预分配 route slot、reader grace、claim state 与
  `IrqNotification`。硬 IRQ 不取得 VM task registry 锁；route registration、worker handle 与
  timer wheel 是纯任务态 owner，使用可睡眠 mutex。永久 per-CPU timer worker 由全局 timer
  service 显式持有强 `ThreadHandle`，而不是 drop handle 后依赖隐式泄漏。

至此任务调度核心及实现该模型所必需的 runtime/Starry/AxVM 边界不再保留旧实现或双轨状态。
后续编译、QEMU 和性能阶段可以修复违反上述不变量的实现错误，但不得以旧字段、轮询推进、
超时重试或 fallback 恢复已删除架构。

### 2026-08-07 rq、ktimer、membarrier 与 lazy-mm 反向核验

统一修错前又按 Linux v7.1 `task_struct::on_rq`、`cfs_rq::curr`、`ktimers/%u`、
`sync_runqueues_membarrier_state()`、`membarrier_switch_mm()` 和 arm64 `enter_lazy_tlb()`
反向检查了一次完整边界，结论与收敛动作如下：

- task 的 `SchedulerPlacement::on_rq`、rq 的 `current` 与 class intrusive node 是 Linux 本身
  定义的三个正交事实，不应错误合并。`RunQueue::membership` 只是 generation-bearing class
  linkage locator，不发布 runnable/current 语义；Fair current 离开 EEVDF tree 后由
  `CurrentClassState::Owned` 持有，RT/DL current 则由 `CurrentClassState::Linked` 指向仍在
  active class 结构中的 entity。二者都只能在同一个 owner-rq transaction 中转换；没有新增
  task-side mirror 或兼容查询；
- `ktimers/%u` 是固定绑定的普通 FIFO kernel thread，不是 hardirq/softirq accounting owner。
  hardirq 只把 generation-bearing timeout 值移入预分配 buffer 并发布 `IrqWaitCell`；worker
  的执行时间正常计入该线程的 `rq->clock_task`。公开的
  `take_current_expired_task_deadlines()` 旁路已经删除，除唯一 ktimer consumer 外，调用方不能
  取得或执行 IRQ 已 claim 的 timeout payload；永久 worker 的 waiter 保存在自身栈帧，不用
  `Box::leak` 延长生命周期；
- membarrier requested/ready 位只保存在共享 `AddressSpaceCpuState`，不同 scheduler token
  通过同一 `Arc` 获得相同 mm identity。注册和 expedited 命令在普通任务上下文预分配 CPU
  mask/lease 容器，再持有 CPU publication read-side lease 选择 rq，最后通过固定 hard-call
  同步目标；CPU offline 不能与该快照交错，因此不存在 `CpuOffline` 重试或本地 CPU fallback；
- `PRIVATE_EXPEDITED` 的权限判断读取当前共享 mm 的 authoritative `READY` 位，而不是读取注册
  同步前刻意只含 `REQUESTED` 的本地 rq cache；后者只负责选择当前正在运行同一 mm 的目标。
  `GLOBAL_EXPEDITED` 与 Linux v7.1 一样不要求调用者自己的 mm 先注册，注册位只决定哪些远端
  rq 参加这次全局 expedited rendezvous。确定性测试在旧实现中分别得到错误的
  `NotRegistered`，修复后保持 mm authority 与 rq target cache 的两种职责正交；
- AArch64 kernel thread 可以像 Linux `enter_lazy_tlb()` 一样保留 previous active-mm lease，
  同时把 TTBR0 切到 reserved root。再次调度同一 mm 时不能仅因 identity 相同而跳过硬件
  恢复；same-mm activation 现在保留原 lease、重新确保 user root 已安装，并由 backend 对
  已正确的 root 消除实际寄存器写。这样 tracker、membarrier identity 与硬件 TTBR0 不再出现
  “逻辑仍 active、实际仍 reserved”分裂。

该复审也否定了两个看似统一、实则偏离 PREEMPT_RT 的修改方向：不得把 ktimer worker 的任务
运行时间扣成 hardirq 时间；不得把 `on_rq`、`rq->curr` 和 class linkage 压成一个无法表示
Fair-current/RT-running 留队差异的枚举。

### 2026-08-07 #1916 接入与优先级迁移索引检查点

基于 `origin/dev@159c16bcb` 重新核对 #1916 的 typed IPI transport，并逐行对照 Linux v7.1
`kernel/sched/rt.c` 的 `pushable_tasks`、`kernel/sched/deadline.c` 的
`pushable_dl_tasks_root` 以及 `kernel/time/hrtimer.c` 的 `softirq_activated`。本检查点得到以下
结论和改动：

- #1916 的 `ax-ipi::DeliveryEdge` 已提供完整的物理 `Idle/Sending/Armed + epoch` 生命周期。
  ax-runtime 原有 scheduler 专用 doorbell 与它重复，现已删除；scheduler、hard-call 和 legacy
  IPI 用户在 handler 入口统一 claim 物理 edge。ax-task 的 generation 仍是逻辑 rq request/ack
  authority，只通过 `notify_scheduler_cpu()` 请求一个物理边，二者不再传递或复制 generation；
- 当前 RT/DL overload publication 虽然已有 cpupri/cpudl，但 rq 内只保存 RT priority bitmap 和
  DL slot membership，真正 push/pull 仍扫描 active class tree。这与 Linux “优先级索引负责选
  候选、active tree 负责 pick”不一致。RT 现使用每 priority 的独立 task-embedded FIFO
  pushable linkage；DL 使用独立 task-embedded、按 absolute deadline 排序的 AVL tree，作为
  Rust 中与 Linux plist/rb-tree 等价的无分配实现。enqueue/dequeue、set-next/put-prev、affinity、
  reclassify 和 migration 都通过 class hook 更新该唯一 membership，balance 不再扫描 running
  entity 或不可迁移任务；
- 原 soft-timer 选择把“queue head 已过期”直接等同于“ktimer worker 已获得 owner”，可能在
  hard IRQ 尚未转移 payload 前停止物理 clockevent。现增加与 Linux
  `hrtimer_cpu_base::softirq_activated` 同义的 owner bit：只有 hard clockevent path promote 后
  才能抑制已到期物理边；worker 完成有界 drain 后清除或重新发布该 bit；
- scheduler/fair deadline selection 改为 pure peek。普通 park arm/cancel 或 deadline
  publication 不得顺便把已到期 Fair timer 转成 pending，也不得以 scheduler work 代替一个尚未
  firing 的硬 timer；已到期 hard deadline继续交给 `LocalClockEvent`，由设备最小 delta 触发唯一
  firing transaction。

本检查点不宣称整体重构完成。后续必须先完成以下架构闭环，再进入小错误和性能修复：

1. 对 RT/DL pushable class hook 做确定性 ordering、current exclusion、affinity change、迁移回滚
   测试，并删除剩余 generic balance filter 与 active-tree fallback；
2. 核验 `OwnerRqTxn` 的 put-prev/pick/set-next、root-domain push iterator 与 switch-tail 在所有
   error path 都只发布一次 cpupri/cpudl/overload；
3. 完成 clockevent `Firing`、ktimer owner bit、idle polling 与 hotplug 迁移的 virtual-runtime/
   loom 覆盖，确认任何进度都不依赖偶然 tick 或 task-context 查询；
4. 统一编译 ax-task、ax-runtime、ax-sync 及 mandatory callers，随后运行 x86 ArceOS/Starry
   QEMU；只有架构闭环后才定位具体测试和性能回退。

### 2026-08-12 RT eligibility 与 runtime ledger 分层

`test-ext4-inode-unique` 的 owner-rq 计数红测证明，一次只更新 rq clock、没有修改 runnable
事实的事务仍会取得一次独立 RT runtime lock。原因不是 RT accounting 本身，而是
root-domain publication 为读取 `throttled` 进入了与 rq 分离的 ledger；Fair-only wake、pick
和 clock transaction 因此都承担一条无关锁链。Linux v7.1 把 `rt_time/rt_runtime/throttled`
放在同一个 `rt_rq` 中，并规定 `rt_runtime_lock` 嵌套在 rq lock 内，但它没有 TGOSKits 这种
“rq 事务完成后再由另一个 facade 重取 ledger 来发布 cpupri/overload”的分裂调用链。不能只把
旧字段复制到 rq 或增加 atomic cache，否则会形成两套 throttle authority。

当前实现按事实用途做一次性迁移，不保留兼容读取：

- `CpuRunQueueState::rt_throttled` 是 class eligibility、cpupri 和 overload publication 的唯一
  authority，只能在 owner rq transaction 内读写；
- per-rq `RtRunQueueBandwidth` 只保存 `rt_time/rt_runtime`、enable state 与 runtime loan，仍由
  嵌套的 IRQ-safe bandwidth lock 保护；root `RootRtBandwidth` 仍独占 period timer、base quota
  和跨 rq loan serialization；
- RT runtime charge 在已经持有 rq lock 时进入 bandwidth ledger，完成 strict
  `rt_time > rt_runtime` 和 Linux 式 runtime sharing，再在释放 rq 前提交 throttle transition；
- period owner 的无锁前置快照必须同时观察 rq throttle bit。`rt_time==0 && !rt_nr_running`
  只能在 rq 也未 throttled 时跳过；否则 optimistic ledger snapshot 与并发 rq publication
  交错会把清除 transition 推迟一个完整 period；
- CPU enable 在 rq -> bandwidth 锁序中同时重置 ledger 和 throttle。CPU disable 先在已经关闭
  publication 的 hotplug 生命周期内回收 loan、禁用 ledger，再清除不可再被 scheduler 观察的
  rq bit；不建立 bandwidth -> rq 的在线反向锁序。

两条确定性红绿测试约束该边界。clock-only owner transaction 在旧实现稳定取得一次 RT
bandwidth lock，新实现为 0；optimistic period snapshot 与随后 rq throttle publication 的模型
在旧 fast path 会留下 `rt_throttled=true`，新实现由同一次 period owner transaction 清除。
既有 strict quota、period unthrottle、普通 RT throttling、PI quota exemption 与 dedicated-idle
replenishment 测试继续约束行为。该阶段只删除无关锁获取并统一事实所有权；端到端耗时仍需用
相同 QEMU case 与 `origin/dev`、Linux PREEMPT_RT 分别复测，不能从锁计数直接宣称性能完成。

### 2026-08-12 wake transaction 的唯一 preempt lifetime

固定 qperf 窗口继续把 `preempt_guard_enter`、`enter_lock_preempt` 指向 direct-wake 热点。完整
公共调用链显示，普通 task-context 的 `ThreadWakeHandle::wake()` 先在 facade 建立
`PreemptScope` 以读取 waker CPU，随后 `TaskSystem::wake_thread_direct()` 又通过
`try_scheduler_activity()` 建立第二个 preempt guard。后者除了稳定 CPU，还负责阻止 thread exit
关闭 scheduler activity gate，因此不能从内部删除；只删内部 guard 会把性能问题改成退出竞态。

Linux v7.1 `try_to_wake_up()` 自身先用 `guard(preempt)()` 稳定 waker CPU，再取得 `p->pi_lock`
串行化 wakee 的 lifecycle、affinity 与 `on_cpu` 等待。调用者不为同一唤醒另建 preempt lifetime。
ax-task 据此把 current CPU 采样移入唯一 scheduler activity transaction：

- `WakerCpuSource::Current` 只能在拿到 `ThreadSchedulerActivity` 后解析；解析接口显式借用该 guard，
  使 waker identity 不能在迁移仍开放时提前采样；
- hard IRQ 或 scheduler frame 已拥有更强 CPU scope 时，activity guard 复用该 scope 的空 token，
  不重复修改 suspended task 的 preempt word；普通 task context 则由 activity guard 建立唯一 token；
- 显式 CPU hint、current-CPU wake 与 wait-claim 只选择 waker identity 的来源，后续都进入同一
  task sched lock、target publication、`on_cpu` Acquire 等待和 target-rq enqueue 算法；没有保留
  facade pin、备用 wake 实现或兼容分支。

公共 facade 回归 `public_wake_owns_one_preemption_lifetime` 在旧实现确定性观测 2 次 preempt
entry，要求同一次 wake 只能观测 1 次；修改后由同一回归验证为 1。该测试约束的是实际
`ThreadWakeHandle::wake()`，不是绕过 facade 的内部 helper。完整 host-test、clippy 与 QEMU 性能
结果在阶段提交前继续验证，不能仅由 guard 计数宣称端到端回退已经消失。

### 2026-08-12 sticky preemption 与物理 IPI edge

固定 qperf 窗口中 wake-preemption decision 明显多于 scheduler IPI send，但 runtime
`DeliveryEdge` 只能合并尚未被 handler claim 的物理边。handler 在入口释放 edge ownership 后，
owner scheduler 可能尚未取得 rq lock 并 claim ax-task 的逻辑 request；这个窗口中的后续 wake
过去会推进逻辑 generation 并再次请求物理 IPI，即使 `REQUEST_PREEMPT` 已经保持 sticky。

Linux v7.1 `__resched_curr()` 在 current 已有 `_TIF_NEED_RESCHED` 时立即返回；Fair
`wakeup_preempt_fair()` 也在 current need-resched 时跳过重复 preemption decision。这里不能把
逻辑判断下放给 `DeliveryEdge`：物理 edge 与 scheduler generation 已有意解耦，runtime 不知道
target owner 是否已经消费逻辑 request。

ax-task 现在以 `REQUEST_PREEMPT` 的 `0 -> 1` transition 作为唯一 preemption publication：

- 第一个 producer 原子推进 generation、设置 sticky bit，并根据 idle-polling/local-owner 状态决定
  是否请求物理 edge；
- bit 尚未被 owner claim 时，后续 producer 不推进 generation、不请求新 edge；
- owner 在 rq transaction 入口通过 `claim_scheduler_request()` 清除 entry bit。此后的新 wake 会
  重新执行 `0 -> 1`、推进 generation 并请求新 edge，因此 claim 与 acknowledge 之间的并发请求
  仍保留给下一次 scheduler pass；
- `REQUEST_OWNER_WORK` 继续按每次有界 producer publication 推进 generation。preemption sticky
  合并不改变 owner-work batch 或 physical `DeliveryEdge` 的所有权。

确定性回归 `pending_preemption_does_not_ring_a_second_doorbell` 先在旧实现观测同一 pending
preemption 发送 2 个 IPI，要求为 1；随后 claim/ack，再发布一个 preemption，必须观察总发送数
变为 2。这样同时约束重复 edge 与丢失新 request 两个方向。完整 ax-task host-test 通过 441 个
unit、全部 integration、21 个 loom 与 12 个 doctest；五组 clippy 通过。

正式 x86_64 `test-ext4-inode-unique` 从上一检查点 guest 84s/QEMU 89.92s 降到
guest 79s/QEMU 84.82s；同机 warm `dev` 为 guest 67s，剩余差距仍约 18%。相同 leaf-qperf
60.657s 窗口推进到同一个 `file-0474`，scheduler IPI send 从 18,751 降到 16,986，consume 从
17,872 降到 16,259；context switch 则从 117,890 变为 118,053，未下降。结果说明 sticky
preemption 合并确实减少物理中断放大，但剩余回退主要仍在真实切换/唤醒链。full-stack qperf
在 shell prompt 前触发 QEMU plugin `SIGSEGV`、没有产生样本，因此不能据该失败构造 caller 结论。

### 2026-08-12 真实 context switch 原因分布

sticky preemption 合并后，IPI send/consume 已下降，但相同 60 秒窗口的 context switch 数没有
下降。仅靠总数无法判断这些切换来自重复 reschedule、主动 yield、迁移，还是 I/O 阻塞后的真实
wake；继续从 leaf hotspot 猜调用者会把队列实现成本和 block runtime 往返混在一起。

`SwitchReason` 已经是跨 OS extension callback 的稳定五值 ABI，且 `execute_switch_plan()` 在
`requires_context_switch()` 过滤后、唯一架构 `switch_context()` 之前拥有最终 reason。qperf 指标
因此只在这个位置计数，不在 scheduler decision、wake 或 switch callback 中建立第二来源：

- 红测 `context_switches_are_classified_by_reason` 在旧行为下得到总数 5，而
  Preempted/Yield/Blocked/Exited/Migrated 全部为 0；
- 修复后一次真实 switch 先增加总数，再按最终 `SwitchReason` 严格增加且仅增加一个分类；
- Starry `/sys/kernel/debug/scheduler_metrics` 只渲染同一 ax-task snapshot。字段是启动以来累计的
  Relaxed 诊断值，工作负载必须以前后快照差分，不能把跨字段读取误作原子事务。

相同 x86_64 Q35/TCG、4 vCPU、1009 Hz leaf-qperf 的 60.605 秒窗口再次推进到
`file-0474`。前后差分为：

| 指标 | 增量 | 占真实切换比例 |
|---|---:|---:|
| context switch | 121,350 | 100% |
| Blocked | 77,168 | 63.591% |
| Preempted | 44,177 | 36.405% |
| Yield | 1 | 0.001% |
| Exited | 4 | 0.003% |
| Migrated | 0 | 0% |
| direct wake activation/enqueue | 77,168 | — |
| scheduler IPI send/consume | 18,346 / 17,504 | — |
| clockevent IRQ | 15,217 | — |

五个 reason 的和精确等于总切换数；Blocked 与 direct-wake activation/enqueue 的窗口增量也相等。
相等本身不能为每个高层 wait source 建立因果映射，但它否定了“迁移、yield 或重复 activation 是
11 万次切换主因”的假设。当前主要成本是大量真实 block/wake 所放大的每次 owner-rq、class queue、
CPU-local/context guard 与 switch handoff 固定工作。qperf 两个相邻检查点的聚合热点也稳定：
sync bridge 约 8.59%、CPU-local/percpu 约 15.02%、scheduler queue 约 6.56%、memcpy/memset
约 3.84%、block runtime 约 9.59%。下一步必须与 `dev` 的相同窗口比较真实切换次数；只有次数
接近而耗时不同，才能把剩余 18% 明确归因到单次调度事务而不是 block 分层本身。

该提交第一次运行 qperf 时曾在 shell prompt 前停顿 150 秒，98.68% boot 样本集中在
`ax_task::sync::bridge::spin_acquire` 的同一 AtomicBool 等待循环。随后同一 ELF/rootfs/plugin 的
两次 GDB-capable 启动与 8 次有界重复启动全部到达 shell，未再次捕获锁地址或 owner，正式 reason
窗口也正常完成。因此这里只保留低频竞态证据，不据一次不可复现停顿修改锁算法，也不把 qperf
进程返回 0 当作该次 guest 启动通过。

### 2026-08-12 dev switch 计数与 ticket unlock

两边原始 qperf 记录都只有定频 PC sample，没有 guest task identity 或 `sched_switch` event，不能从
`qperf.bin` 反推真实切换次数。为避免把采样占比当事件数，`dev@fad09ebd3` 的临时诊断构建只在
旧实现唯一真实 `AxRunQueue::switch_to()`、且 `prev != next` 后增加计数，并通过 debugfs 在同一
workload 窗口前后读取。该补丁未进入 PR，诊断后已还原干净 worktree。

相同 x86_64 Q35/TCG、4 vCPU、1009 Hz leaf-qperf 的 60 秒窗口结果为：

| 实现 | 窗口 | 最后观测进度 | 真实 switch 增量 | host user | scheduler/wait/preempt leaf |
|---|---:|---:|---:|---:|---:|
| current，修复前 | 60.605s | `file-0474` | 121,350 | 88.154s | 46.758% |
| `dev@fad09ebd3` | 60.636s | `file-0666` | 217,773 | 72.312s | 31.509% |

两边都由 workload timeout 截断，不能把约 60 秒窗口误报为完成耗时。`dev` 在观测到更多文件进度
的同时执行了约 1.79 倍真实 switch，却只消耗约 82% host user CPU；因此剩余回退不是 current
制造了更多切换，而是每次真实 block/wake/switch 放大的固定实现成本。timer/clockevent leaf 在
两边都约 1.21%，且 `dev` 的 `timer_set_deadline_in_ticks` 样本更多，也不支持把主因归给 current
统一 selection tail 的 deadline 检查。

owner rq 和 task scheduler state 使用的 `RawTicketLock` 在取 ticket 时必须执行一次 atomic RMW，
但旧 unlock 又用 `owner.fetch_add(Release)` 执行第二次 RMW。holder 是唯一可推进 `owner` 的上下文；
waiter 只 acquire-load。Linux generic ticket spinlock 与 queued spinlock 的 unlock 都是
`smp_store_release`，并不要求第二次 RMW。因此红测 `uncontended_unlock_uses_release_store` 先在旧
策略稳定得到 `ReadModifyWrite`、期望 `Store`；修复删除 RMW 分支，只保留 Relaxed 读取当前 owner
后 Release-store successor 的实现。failed try-lock rollback、4×1000 concurrent writers、ax-task
完整 host/qperf、loom 与 clippy 均通过。

修复后的同配置 qperf 窗口为 60.621 秒，仍只到 `file-0474`，真实 switch 增量 122,825，host user
88.124 秒，scheduler/wait/preempt leaf 47.324%。因此 release-store 是四架构都需要的正确锁语义与
固定成本修复，但本次结果没有可见吞吐收益，不能把它标记为剩余回退主因。下一层证据必须量化
native lock 的 preempt guard、owner-rq IRQ guard、class queue 操作和 runtime CPU-area lookup，
而不是继续微调 ticket lock 或添加 ext4/Fair workload 特判。

### 2026-08-12 guard 与 owner-rq 事务计数

为区分 rq 事务本身与外围 guard 固定成本，`qperf-metrics` 将所有生产代码的 runtime preempt/IRQ
guard 入口收敛到各一个内部计数点，并按 provider 是否返回空 token 计数；`OwnerRqTxn` 则按普通
irq-save、scheduler-frame 和 bootstrap 三种 typed constructor 分别计数。计数只观察现有所有权
协议，不新增 guard、锁实现或 OS 侧状态镜像。完整 ax-task host/qperf、loom、doctest 和 ax-task、
starry-kernel clippy matrix 均通过。

相同 x86_64 Q35/TCG、4 vCPU、1009 Hz leaf-qperf 的 60.622 秒窗口仍到 `file-0474`。前后差分为：

| 指标 | 增量 | 相对真实 switch |
|---|---:|---:|
| runtime preempt guard entry | 766,816 | 6.412 次/switch |
| preempt guard empty token | 391,705 | entry 的 51.082% |
| runtime IRQ guard entry | 2,950,834 | 24.676 次/switch |
| IRQ guard empty token | 0 | entry 的 0% |
| owner rq irq-save transaction | 140,332 | 1.174 次/switch |
| owner rq scheduler transaction | 153,977 | 1.288 次/switch |
| context switch | 119,582 | 1 |
| Blocked / Preempted | 76,593 / 42,984 | 64.051% / 35.945% |
| direct wake attempt / activation | 95,274 / 76,595 | — |

该运行中的 Relaxed 全局计数本身增加热路径开销，因此不与未插桩窗口比较 host CPU 或吞吐；这里只
使用同一窗口内的结构比例。约 29.4 万次 owner-rq transaction 没有解释约 295 万次 IRQ guard
entry，且调用链审计没有发现同一 owner rq 已持有 transaction 后又无条件重新 `begin`。主要放大
发生在 rq transaction 之外或 transaction 内的其他 typed locks/guards，而不是 transaction
constructor 重复。

`runtime_irq_guard_none == 0` 也不能成为“IRQ 已关闭就跳过 guard”的依据：每次 entry 都取得真实
runtime IRQ owner，`RuntimeIrqGuard` 还绑定当前 CPU handle，`IrqTicketLock` 则用 move-only scope
恢复精确 IRQ 状态。Linux v7.1 同样通过 `raw_spin_rq_lock_irqsave()`、已持有 baton 的 raw rq
variant 与 `task_rq_lock()` 的分层接口表达所有权，不按现场 IF 状态猜测。下一检查点必须在 typed
acquisition source 处区分 `RuntimeIrqGuard`、executor publication、各类 `IrqTicketLock` 和显式
`IrqScope`，同时区分 public spin、scheduler activity、PI/wait 等 preempt guard；只有证明某类
调用已经借用更强 baton，才允许从接口层删除重复 transaction，不能增加兼容快路或第二套锁实现。

source-classified 计数保持每次 entry 仍只更新一个 entry counter，总数由 snapshot 对分类求和；因此
没有在热路径叠加第二次总数原子操作。相同配置的 60.617 秒复测仍到 `file-0474`，真实 switch
增量为 117,697，分类差分为：

| guard source | entry 增量 | 占同类 entry | empty token | empty/entry |
|---|---:|---:|---:|---:|
| preempt ticket lock | 341,352 | 46.260% | 271,112 | 79.423% |
| preempt explicit scope | 147,618 | 20.005% | 0 | 0% |
| preempt public/native sync | 138,050 | 18.708% | 86,924 | 62.966% |
| preempt scheduler activity | 110,884 | 15.027% | 29,513 | 26.616% |
| preempt IRQ-return | 0 | 0% | 0 | — |
| IRQ ticket lock | 2,516,167 | 91.447% | 0 | 0% |
| IRQ explicit publication scope | 57,387 | 2.085% | 0 | 0% |
| IRQ runtime CPU baton | 177,799 | 6.462% | 0 | 0% |
| IRQ executor publication | 162 | 0.006% | 0 | 0% |

IRQ ticket lock 平均每次真实 switch 进入 21.378 次，是 275.2 万次 runtime IRQ guard 的决定性
来源；executor 与通用 runtime CPU facade 不是主要放大源。preempt 侧则不是单一 public spin：
内部 ticket lock 占 46.260%，其中近八成已经在更强 scheduler/IRQ owner 下返回空 token；public/native
sync 与 scheduler activity 也分别存在 86,924 和 29,513 次空 token。空 token 仍然经过 runtime
provider/CPU-local ownership 判断，因此下一步需要按 ticket lock 所保护的 authoritative state 与
typed caller baton 继续分类。此时不能把 `IrqTicketLock` 或 `PreemptTicketLock` 整体替换为 raw：
普通 task context 仍需建立真实 exclusion，只有 API 已持有 scheduler/IRQ baton 的调用点才能选择
raw/no-pin variant，这与 Linux `rq_lock_irqsave()` 和已持有 baton 的 raw rq 接口分层一致。

### 2026-08-12 root RT period active publication

按 authoritative state 继续拆分后，60 秒窗口中的 2,811,593 次 IRQ ticket acquisition 可以完整
归因：thread sched 531,269，CPU rq 1,372,637，per-rq RT bandwidth 25,438，CPU deadline base
641,087，root RT period 241,162，root RT runtime 与 root deadline index 均为 0。root period 在没有
显式 task deadline event 的窗口内平均每次真实 switch 进入约 2.05 次，因此先用两项确定性红测分别
覆盖 inactive deadline observation 和 inactive callback claim；旧实现连续 128 次操作都进入 128 次
IRQ guard，期望为 0。

Linux `rt_bandwidth` 在 root state 中维护 `rt_period_active`：start 在 runtime lock 内完成 timer 身份后
置 active，idle callback 清 active；inactive 查询不需要进入 root lock。`RootRtBandwidth` 因此只增加
一个 derived `AtomicBool`：owner、deadline、generation 和 firing 仍由原 `IrqTicketLock` 唯一拥有；
activation 在锁内先完成权威状态，再 Release 发布 active；观察和 callback 以 Acquire 读取 false 后
直接返回，true 仍回到权威锁复核；idle callback 清空权威状态后撤销 publication。没有复制 timer
identity、增加第二套锁或保留旧接口。两项红测均转绿，active -> firing -> idle 的生命周期回归也确认
撤销后两条路径都不再进入 IRQ guard。

该修改没有被当作性能问题已经解决。相同 4-vCPU Q35/TCG、1009 Hz qperf 复测仍只到
`file-0474`，root period ticket 增量为 234,620，基线为 241,162；CPU deadline 与 rq ticket 等其他
分类也同步低约 2%，host user 仅从 87.837 秒降到 87.311 秒。两次窗口都由 workload timeout 终止，
没有 stop marker，不能把这组小幅共同比例变化归因于 active gate。代码审计也解释了为何本 workload
不是 inactive：每 CPU `ktimers/%u` 是 FIFO RT worker；period callback 只要 replenish 后
`time_ns != 0`、rq 仍 throttled 或仍有 current/queued RT member 就继续 active。下一阶段必须量化
CPU deadline base 与 rq 的具体 acquisition source，不能再把 root period 总数解释成空状态查询，
也不能以本次正确但无可见吞吐收益的 publication 修复替代剩余性能根因。

### 2026-08-12 CPU deadline base acquisition source

为避免把 CPU deadline base 的 64 万次 acquisition 当成单一问题，所有生产调用点先按权威状态
转换分类为 observation、publication、registration、hard expiry、soft expiry 和 lifecycle。分类仍沿用
每次 entry 只更新一个计数器的约束，总数由 snapshot 求和，不在锁热路径叠加第二次总数原子操作；
每个 `lock_deadline_base` 调用点必须显式提供来源，不保留 unknown/default 兼容入口。

相同 x86_64 Q35/TCG、4 vCPU、1009 Hz 的 60.805 秒精确 marker 窗口仍到 `file-0474`，workload
由内部 60 秒 timeout 终止，但 stop marker、QMP quit 与 qperf 窗口均完整。CPU deadline base 的
637,975 次增量可以完整归因：

| deadline base source | entry 增量 | 占 CPU deadline base |
|---|---:|---:|
| observation | 245,434 | 38.472% |
| publication | 219,249 | 34.366% |
| registration | 71,261 | 11.171% |
| hard expiry | 15,398 | 2.414% |
| soft expiry | 86,633 | 13.579% |
| lifecycle | 0 | 0% |

窗口内真实 switch 增量为 124,001，CPU deadline base 平均每次 switch 进入 5.145 次；其中 observation
与 soft/hard expiry 合计 347,465 次，占 54.464%。下一阶段先用确定性回归覆盖空 base 的观察和
expiry 探测，再参照 Linux hrtimer `active_bases` 的派生 publication，在 false 时跳过权威锁、true 时
仍入锁复核。publication 负责物理 clockevent generation/deadline 的唯一所有权，registration 负责
队列状态转换；不能仅因为 `task_work_deadline_events` 为 0 就删锁或复制 deadline identity。

确定性回归先在真实 1-CPU facade fixture 的空 base 上执行两次 next-event observation、一次 hard
expiry probe 和一次 soft expiry probe；旧实现稳定得到三类 IRQ ticket 增量 `(2, 1, 1)`，期望
`(0, 0, 0)`。修复把 derived active publication 与唯一 `CpuDeadlineBase` owner 封装在一起：
Registration、HardExpiry 和 SoftExpiry 只能通过 activity guard 修改 queue、expired buffer/count 或
softirq ownership，guard 在原锁释放前按三者的权威状态 Release 发布；false fast path 以 Acquire
拒绝，true 时仍进入原锁复核。普通 observation 只取得不可变 guard；旧通用可变
`lock_deadline_base` 接口被删除。物理 clockevent generation/publication 使用独立固定入口，不重写
active，也没有第二套 timer identity、锁算法或兼容路径。同一红测转绿，完整 host/qperf、loom、
doctest 和 clippy 均通过。

相同配置的两个 60.8 秒修复后窗口继续使用真实 switch 归一化。相对修复前每 switch 5.145 次
CPU deadline-base acquisition，两次分别为 4.574（-11.097%）和 4.714（-8.382%）；observation
分别下降 23.098% 和 20.852%，hard expiry 分别下降 19.466% 和 18.480%。publication 分别变化
-1.360% 和 +0.732%，证明物理 deadline publication 没有被错误绕开。第一轮推进到 `file-0538`，
第二轮仍只到与基线相同的 `file-0474`；host user 分别为 88.032 秒和 88.365 秒，基线为
87.882 秒。因此这里只确认空 base 锁放大被稳定消除，不把单次吞吐改善解释成剩余性能问题已经
解决；下一阶段继续量化 CPU rq 和 thread scheduler state 的 acquisition source。

### 2026-08-12 CPU runqueue acquisition source

CPU runqueue 的普通 IRQ-save 入口继续按权威状态转换拆成 transaction、owner observation、timer
observation、RT accounting、deadline accounting、membarrier 和 lifecycle；已经持有 scheduler
baton 或处于 offline bootstrap 的 raw 入口不混入普通 IRQ ticket 统计。所有生产调用点必须显式
提供来源，不保留无参数或 unknown/default 兼容入口；总数仍由七类 snapshot 求和，每次 acquisition
只增加一个计数器。

相同 x86_64 Q35/TCG、4 vCPU、1009 Hz 的 60.928 秒精确 marker 窗口推进到 `file-0474`，内部
workload timeout 后仍完整输出 stop marker 并由 QMP 退出。CPU runqueue 的 1,298,480 次普通
IRQ-save acquisition 可以完整归因：

| runqueue source | entry 增量 | 占 CPU runqueue | 每次真实 switch |
|---|---:|---:|---:|
| transaction | 139,706 | 10.759% | 1.185 |
| owner observation | 704,144 | 54.228% | 5.973 |
| timer observation | 445,210 | 34.287% | 3.777 |
| RT accounting | 9,404 | 0.724% | 0.080 |
| deadline accounting | 0 | 0% | 0 |
| membarrier | 0 | 0% | 0 |
| lifecycle | 16 | 0.001% | 0.0001 |

窗口内真实 switch 增量为 117,884，runqueue 普通 IRQ-save acquisition 平均每次 switch 进入
11.015 次。owner/timer observation 合计 1,149,354 次，占 88.515%；真正改变 current、队列或
调度类状态的 transaction 只有 10.759%。这排除了“Linux 等价的 rq transaction 本身就是主要
放大源”的解释，也不能据此绕过 rq lock：Linux `task_rq_lock()`、`rq_lock_irqsave()` 和 scheduler
owner 下的 raw rq lock 仍分别保护 task placement 与权威 runqueue 状态。下一阶段必须把两类
observation 继续拆到具体入口，判断哪些只是查询 derived publication、哪些确实读取 current/queue
权威状态；在此之前不增加 lockless mirror，也不改变事务路径的锁语义。本轮 host user 为
88.386 秒，吞吐与 CPU 时间均未改善，因此这里只确认放大来源，不宣称性能问题已经解决。

随后把 owner observation 按 current-thread、current-core、current-handle、idle、runnable 五个语义
入口拆分，把 timer observation 按 scheduler-clock-event 与 fair-balance 两个入口拆分。原 owner/
timer 字段仅由 leaf 求和，不再占用独立 source；每次锁进入仍只记录一个 leaf。相同配置的 60.955 秒
窗口再次推进到 `file-0474`，host user 为 88.026 秒。活跃系统上的 Relaxed debugfs 前后读取使
aggregate 增量与 leaf 求和相差 270（0.020%），以下使用 1,350,515 次 leaf acquisition 归因：

| runqueue leaf source | entry 增量 | 占 leaf 总数 | 每次真实 switch |
|---|---:|---:|---:|
| transaction | 143,503 | 10.626% | 1.179 |
| owner current thread | 170,757 | 12.644% | 1.403 |
| owner current core | 260,925 | 19.321% | 2.144 |
| owner current handle | 102,714 | 7.606% | 0.844 |
| owner idle | 199,626 | 14.781% | 1.640 |
| owner runnable count | 0 | 0% | 0 |
| timer scheduler clock event | 231,627 | 17.151% | 1.903 |
| timer fair balance | 231,627 | 17.151% | 1.903 |
| RT accounting | 9,720 | 0.720% | 0.080 |
| lifecycle | 16 | 0.001% | 0.0001 |

窗口内真实 switch 增量为 121,711，物理 clockevent IRQ 增量为 15,207。两个 timer leaf 不仅完全
相等，而且各自平均每个物理 IRQ 进入 15.232 次；因此不能把它们解释为“一次 IRQ 各观察一次”。
调用图确认 `on_clock_event` 自身只在末尾推导一次，但 switch、enqueue、park、ktimer 等状态转换也
各自要求刷新物理 deadline；真正确定的重复是每次推导内部都为 current runtime 与 fair balance
分别取得 rq lock。owner 侧的
`runnable_count` 在该 workload 为 0，不能用它解释原来的 54% owner observation；主要来源是
current-core、idle、current-thread 和 current-handle 的分散查询。

Linux 允许 `READ_ONCE(rq->curr)` 只作启发式选择并在持锁事务中复核，但 `rq->nr_running`、current/
idle 多字段一致性、当前实体 runtime 以及 `rq->next_balance` 都没有通用 lockless snapshot；这些
状态仍需 rq lock 或一项有明确生产者协议的 derived publication。因此下一阶段先合并同一 deadline
推导中的 runqueue observation，并继续分类造成推导次数远高于物理 IRQ 的非 IRQ 状态转换，不能
直接把上述七个入口改成无锁读取，也不能复制 current/queue 状态。

确定性红测在一个 current fair task 与一个 contender 的真实 1-CPU fixture 上单独调用一次
`next_oneshot_deadline`；旧实现 timer rq ticket 精确增加 2，契约要求同一推导只进入一次 rq scope。
修复增加一次性 `SchedulerDeadlineRqObservation`：在唯一 rq guard 内共同读取 current/idle、当前
实体 runtime、RT quota 与 periodic fair predicate，然后由 `scheduler_work_due` 或
`next_oneshot_deadline` 消费；原 scheduler-clock-event/fair-balance 两个 guard source 被删除并
替换成唯一 deadline-derivation source，没有保留兼容入口或复制 rq 状态。该顺序对应 Linux hrtick
callback 在一次 rq lock 内完成 `update_rq_clock()` 与 class `task_tick()`，而不是把同一 current
状态拆成多个锁外查询。红测转绿，完整 ax-task qperf-feature、integration、loom、doctest 与目标
clippy 均通过。

相对修复前每 switch 11.098 次总 rq acquisition、3.806 次 timer observation，两个修复后窗口为：

| qperf window | 总 rq / switch | 变化 | timer / switch | 变化 | workload 进度 | host user |
|---|---:|---:|---:|---:|---|---:|
| coherent run 1 | 9.188 | -17.211% | 1.896 | -50.190% | `file-0538` | 88.424 s |
| coherent run 2 | 8.998 | -18.921% | 1.863 | -51.042% | `file-0538` | 87.683 s |

修复前窗口推进到 `file-0474`，host user 为 88.026 秒。两轮都稳定消除了每次推导的第二次 rq lock，
并都多推进一个 64-file 诊断边界；host CPU 时间一升一降，仍不能解释为稳定改善，workload 也仍由
60 秒 timeout 终止。剩余 timer derivation 约 1.86 次/switch，需要继续按 clockevent、switch、
enqueue、park 与 ktimer 触发源分类；剩余 owner observation 约 5.9--6.0 次/switch，则需在保持
task-sched -> rq 锁序与事务复核的前提下合并分散的 current/core/idle 查询。

下一轮只在唯一 `scheduler_deadline_publication()` 入口记录请求重新推导的外层状态转换；八个 leaf
求和得到 aggregate，指标不参与 deadline、generation、timer update 或调度选择。相同配置的
60.954 秒完整 marker 窗口中，219,906 次 scheduler deadline derivation 可以完整归因：

| deadline derivation trigger | entry 增量 | 占全部推导 | 每次真实 switch |
|---|---:|---:|---:|
| clock event | 15,008 | 6.825% | 0.120 |
| park arm | 33,687 | 15.319% | 0.269 |
| park cancel | 28,169 | 12.810% | 0.225 |
| ktimer service | 5,395 | 2.453% | 0.043 |
| enqueue | 0 | 0% | 0 |
| placement | 0 | 0% | 0 |
| schedule selection | 125,508 | 57.074% | 1.001 |
| schedule no-switch | 12,139 | 5.520% | 0.097 |

窗口内真实 switch 增量为 125,415，推导总数为 1.753 次/switch；物理 clockevent IRQ 增量为
14,999，与 clock-event 推导 15,008 基本一一对应。`ScheduleSelection` 才是唯一接近每次 switch
一次的主要来源：selection 已经在 `OwnerRqTxn` 内决定 next 并提交 authoritative rq 状态，随后
`finish_owner_selection()` 释放 transaction，又由 `program_local_timer()` 为相同 current/runtime
事实重新取得 rq observation guard。该边界与 Linux 不一致：Linux 的 `set_next_task_fair()` /
`set_next_task_dl()` 在 rq lock 内按 `first` 产生 hrtick 请求，`hrtick_schedule_exit()` 再以 queued
状态和 5 us expiry 差值合并物理重编程；它不会在 selection transaction 结束后为了读取同一 rq
事实再锁一次 rq。

因此下一确定性红绿阶段不是跳过 `ScheduleSelection` 的 deadline 更新，而是让 selection transaction
在持有唯一 rq guard 时产生 scheduler deadline observation，commit 后只发布已经得到的结果。park
arm/cancel 仍是 deadline queue registration/cancellation 的独立事务，不能假装与 owner-rq selection
共享一把锁；后续只允许通过已有 publication equality 合并物理更新。本轮总 rq acquisition 为
9.017 次/switch，仍推进到 `file-0538`，host user 为 87.799 秒；这里只定位到结构性二次 rq
acquisition，不宣称整体性能问题已经解决。

确定性回归随后构造两条真实 Fair 线程并执行一次 yield selection，同时约束两个事实：
`ScheduleSelection` deadline derivation 必须仍增加 1，而 transaction 释放后的 timer-rq acquisition
必须为 0。旧实现稳定得到 `1/1`；修复让 `OwnerRqTxn` 在最终 current/class/runtime 状态已经形成、
但 rq guard 仍然持有时产出唯一 `SchedulerDeadlineRqObservation`，同一测试转绿为 `1/0`。这份
observation 只保存 due/相对 runtime delay 与 periodic fair predicate，不复制 current、runqueue 或
timer identity；selection tail 以 scheduler completion 的 monotonic time 把相对 delay 转为绝对
deadline，因此没有把 rq observation 到物理 rearm 之间的执行时间漏掉。

所有 schedule、yield、park、exit 与 no-switch 提交点都走同一 observation 边界。可选 balance pass
返回 typed `OwnerBalanceOutcome`：只有 RT/Deadline/Fair 实际迁移并改变本地 rq 时，tail 才通过原
通用路径重新派生；idle-pull request 或无候选 balance 继续使用 transaction observation。没有保留
旧 selection 重取 rq 的兼容路径。两个并行回归使用各自 fixture 的 `CpuRemote` 测试计数，避免全局
qperf 计数被其他 fixture 污染；这些字段只在 `cfg(test)` 下存在，production 布局和热路径不变。
完整 ax-task qperf-feature 测试通过（443 unit、全部 integration、21 loom、12 doctest），ax-task 与
Starry 31 项 clippy 全部通过。

相同 x86_64/4-vCPU/1009 Hz marker 窗口的结构对照如下；两轮 workload 都由 60 秒 timeout 以状态
143 终止，start/stop marker 完整：

| qperf window | deadline derivation / switch | timer rq / switch | 总 rq / switch | owner observation / switch | workload 进度 | host user |
|---|---:|---:|---:|---:|---|---:|
| 修复前 | 1.753 | 1.873 | 9.017 | 5.933 | `file-0538` | 87.799 s |
| transaction observation | 1.773 | 0.853 | 8.099 | 6.000 | `file-0474` | 87.611 s |

修复前后 deadline derivation 仍分别为 219,906/214,483，说明没有靠跳过 timer update 降低计数；
timer-rq acquisition 从 234,880 降至 103,196，每 switch 下降 54.452%，总 rq 每 switch 下降
10.188%。候选窗口中的 `ScheduleSelection` 与 `ScheduleNoSwitch` 合计仍有 133,839 次推导；扣除
80,644 次非 selection 推导和约 14,903 次 clock-event 前置 rq due observation 后，只剩约 7.65k
次 timer-rq acquisition，与“balance 实际迁移后必须重读已改变 rq”的保留分支一致。该分解来自
调用边界与现有计数的合并推断，后续若继续优化这一小部分，必须先增加 balance outcome 分类，不能
把它当作无条件重复删除。

本轮 workload 只推进到 `file-0474`，没有复现前一轮 `file-0538`；host user 基本不变。因此这里只
确认 Linux rq-lock 边界不一致和每 switch 约一次额外 rq acquisition 已被消除，不把单轮结构下降
宣称为吞吐改善，更不能认为相对 dev 的整体性能问题已经解决。剩余最大项重新集中到 owner current/
core/handle/idle 分散 observation（约 6.0 次/switch）；下一阶段需要沿完整调用链判断哪些查询属于
同一 owner transaction、哪些只是 Linux `READ_ONCE(rq->curr)` 级启发式，再建立确定性红绿，不能
直接增加 current/rq 的 lockless mirror。

继续按 Linux execution identity 与 scheduler identity 分层审计后，确认 `CurrentThreadPublication` 已是
本分支的 architecture-selected task current：identity 和 owner Arc 在上下文可执行期间跨抢占、迁移
保持不变。`begin_current_park_with_permit` 却仍通过 `CpuLocal::current_thread_handle()` 锁 rq 并克隆
`rq.current_core()`，把执行身份错误放在 scheduler-owned `rq->curr` 下；随后 `prepare_park` 还会为
park 状态读取 rq。Linux x86 的 `current` 来自 per-CPU `current_task`，`rq->curr` 只在 rq lock 下用于
选择和切换，两者不能因为当前恰好相等就合并所有权。

确定性回归要求真实 facade park 入口读取一次 task current publication。旧实现稳定为 0，修复把
handle 捕获移动到 CPU owner/IRQ transaction 之前后转绿为 1；publication 契约保证这一强 handle
在捕获与随后可能迁移后的 `prepare_park` 之间仍属于同一 task。`prepare_park` 继续在当前 owner CPU
上验证 rq/thread 状态并发布 `Parking`，没有用 task current 替代 rq 权威状态。旧
`CpuLocal::current_thread_handle()`、rq source、IRQ source、qperf leaf 与 Starry debugfs key 已全部
删除，不保留恒零兼容字段或第二条 handle 路径。完整 ax-task qperf-feature 测试通过（444 unit、全部
integration、21 loom、12 doctest），ax-task 与 Starry 31 项 clippy 全部通过。

相同 marker 窗口验证该 acquisition 没有转移到另一种 rq observation：

| qperf window | owner observation / switch | current core / switch | current thread / switch | idle / switch | 总 rq / switch | workload 进度 | host user |
|---|---:|---:|---:|---:|---:|---|---:|
| rq handle | 6.000 | 2.127 | 1.396 | 1.644 | 8.099 | `file-0474` | 87.611 s |
| task current | 5.130 | 2.110 | 1.374 | 1.645 | 7.196 | `file-0474` | 88.125 s |

修复前 handle leaf 为 100,756 次、0.833 次/switch；删除后 owner observation 每 switch 下降
14.502%，总 rq 每 switch 下降 11.142%，而 current-core/current-thread/idle 均未上升到接收这批
查询。两轮都由 60 秒 timeout 以状态 143 终止、完整观察 `file-0474`，host user 反而略升，因此
仍只确认 task-current 所有权修正与 rq acquisition 消除，不宣称吞吐改善。剩余 owner observation
中 current-core 为 2.110 次/switch、idle 为 1.645、current-thread 为 1.374；下一阶段优先处理
schedule/yield/park 在 transaction 前为取得 `previous_sched` 而读取 current core、进入 transaction
后又权威复核 `rq->curr` 的锁序问题，不能把后一次复核删除或增加 current-core mirror。

继续对照 Linux v7.1 `__schedule()` 后，调度入口的 previous-task 所有权也收敛为两层：runtime
facade 从架构 `CurrentThreadPublication` 捕获强 `ThreadHandle`，只用它在 rq transaction 前取得
task-owned `ThreadSchedState` 锁；owner transaction 内仍从唯一 `rq->curr` 取得 thread/core/endpoint
并做 Arc identity 致命复核。`schedule_if_requested`、`yield_current` 和有 previous task 的普通
`schedule` 均显式接收该 handle，不再先调用 `CpuLocal::current_core()`；CPU 初次 dispatch 只能
显式传 `None`，且 transaction 若观察到已有 current 就触发不变量。这样保持现有
task-scheduler-state -> rq 锁序，又没有把 architecture current 升格为 `on_rq/on_cpu` 或调度选择
权威。确定性 facade 回归在旧实现观察到 task-current publication 读取 0，修复后为 1。

完整 ax-task qperf-feature 测试通过（445 unit、全部 integration、21 loom、12 doctest）；ax-task/
Starry 31 项 clippy 及复跑的 ax-task 5 项 clippy 均通过。并行验证最初暴露测试 helper 通过
registry 重建 current handle，恰被“持有冷 registry 锁时 owner schedule 仍需进展”的测试阻塞；
helper 已改为只在 `cfg(test)` 下从当前 core 构造 handle，生产 facade 始终使用架构 publication。

相同 x86_64 Q35/TCG、4 vCPU、1009 Hz、60 秒 ext4 marker 窗口的 qperf 结果如下。profile 必须
使用 `--test-case qemu/system/test-ext4-inode-unique` 注入 grouped-case rootfs；`--case` 只命名
报告，不能替代 test-case 选择。

| qperf window | owner observation / switch | current core / switch | current thread / switch | idle / switch | timer rq / switch | 总 rq / switch | workload 进度 | host user |
|---|---:|---:|---:|---:|---:|---:|---|---:|
| park handle task current | 5.130 | 2.110 | 1.374 | 1.645 | 0.826 | 7.196 | `file-0474` | 88.125 s |
| scheduler task current | 4.722 | 1.664 | 1.407 | 1.652 | 0.857 | 6.830 | `file-0474` | 88.142 s |

current-core 每 switch 下降 21.159%，owner observation 下降 7.939%，总 rq 下降 5.090%；
current-thread、idle、timer-rq 和 owner transaction 只在约 0.4%--3.8% 的同轮噪声范围内变化，
没有接收被删除的 current-core 查询。两轮 workload 都以状态 143 在完整 marker 内结束、只到
`file-0474`，host user 几乎完全相同，因此仍只确认锁获取结构修正，不宣称吞吐改善。current-core
还剩 1.664 次/switch，说明 park commit、exit、affinity 等非 schedule/yield 路径仍在 transaction
前查询 rq core；下一阶段继续逐个建立红绿并传递已有 task-current handle，transaction 内复核不得
删除。

park prepare/commit 随后也改为显式接收同一个强 `ThreadHandle`。runtime
`PreparedCurrentPark` 和 PI park attempt 从 architecture `CurrentThreadPublication` 捕获并持有该
handle，prepare、commit、cancel 全程传递；低层 `TaskSystem` 直接使用新的破坏性接口，不保留无
handle 重载、rq 查询兼容分支或第二套 current 状态。handle 只在 `ThreadSchedState` 锁下用于
task-owned 状态：prepare/cancel 验证 `Running/Parking` 与 `execution_cpu/on_cpu`，commit 用同一
Arc 取得 previous task lock。真正的 current thread/core、generation、Arc identity、block 与 next
selection 仍由后续 owner-rq transaction 权威复核，锁序继续为 thread-sched -> rq。

两个 per-`CpuRemote`、仅 `cfg(test)` 的确定性回归分别约束 park prepare 和 commit 不得在 rq
transaction 前调用 `CpuLocal::current_core()`；旧实现均稳定得到额外 current-core rq acquisition
`1`，修复后同一测试均为 `0`。完整 `cargo test -p ax-task` 通过（442 unit、全部 integration、
21 loom、12 doctest），ax-task 5 项 clippy 全部通过。

相同 x86_64 Q35/TCG、4 vCPU、1009 Hz、60 秒 ext4 marker 窗口确认被删除的查询没有转移到
transaction、idle 或 deadline 路径：

| qperf window | owner observation / switch | current core / switch | current thread / switch | idle / switch | transaction / switch | timer rq / switch | deadline derivation / switch | 总 rq / switch | workload 进度 | host user |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---|---:|
| scheduler task current | 4.722 | 1.664 | 1.407 | 1.652 | 1.172 | 0.857 | 1.777 | 6.830 | `file-0474` | 88.142 s |
| park explicit task current | 2.238 | 0.002 | 0.587 | 1.649 | 1.170 | 0.861 | 1.778 | 4.350 | `file-0474` | 88.055 s |

两轮分别观察 121,623/123,071 次 switch。current-core 每 switch 下降 99.909%，current-thread 下降
58.259%，owner observation 下降 52.615%，总 rq 下降 36.310%；transaction 下降 0.161%，idle
下降 0.171%，timer-rq 上升 0.512%，deadline derivation 上升 0.066%，后四项保持同一结构水平，
没有接收被删除的 identity 查询。两轮 workload 都由 60 秒 timeout 以状态 143 结束且只推进到
`file-0474`，host user 也基本不变，所以该阶段只确认 task-current/rq-current 分层消除了大量无效
rq acquisition，仍不宣称吞吐改善或相对 dev 的性能问题已经解决。下一步继续审计 exit、affinity
和剩余 current-thread/idle observation，并以相同红绿和 qperf 边界验证。

current exit 也按相同 execution-identity/rq-identity 边界完成收敛。runtime facade 在 CPU owner/IRQ
transaction 前从 `CurrentThreadPublication` 捕获 handle；prepare 只用它访问 task-owned scheduler
state，并把同一 core Arc 固定在 move-only `CurrentExitPermit`。dedicated idle 只比较一次安装、
Release/Acquire 发布的 immutable idle identity；commit 删除 transaction 前的 `cpu.current()` 和
`cpu.current_core()`，仍在 owner-rq transaction 内以 current/core Arc 做权威复核。scheduler
activity close/seal、PI/callback 校验、switch-tail 和 reap 顺序均未改变，也没有旧接口重载。

这里不能把完整 `ThreadHandle` 保留到 `Exited` 发布之后：它携带 external lookup lease，随后 drop
会在物理 switch tail 前制造 reap task-work。完整测试确实确定性捕获了这个初版错误。最终 permit
只保存普通 core Arc，facade lookup handle 在任务仍为 `Running` 时释放；纯 scheduler
`exit_current` 同样消费并提前释放 lookup handle，再提交 exit。这保持了 Linux
`do_task_dead()` 到 `finish_task_switch()` 之间 outgoing stack 仍 `on_cpu`、不能提前回收的边界。

确定性回归中，旧 prepare 的 current-thread/current-core/idle rq observation 为 `1/1/1`，修复后
为 `0/0/0`；旧 commit 还有一个 transaction 前 current/core 查询，修复后只剩后续 idle-pull 的
current recheck。完整 ax-task 测试通过（444 unit、全部 integration、21 loom、12 doctest），5 项
clippy 全部通过。

相同 ext4 marker 窗口的单轮对照如下：

| qperf window | current thread / switch | current core / switch | idle / switch | transaction / switch | timer rq / switch | deadline derivation / switch | 总 rq / switch | workload 进度 | host user |
|---|---:|---:|---:|---:|---:|---:|---:|---|---:|
| park explicit task current | 0.587 | 0.0015 | 1.649 | 1.170 | 0.861 | 1.778 | 4.350 | `file-0474` | 88.055 s |
| exit explicit task current | 0.558 | 0.0023 | 1.597 | 1.147 | 0.844 | 1.760 | 4.223 | `file-0538` | 87.197 s |

本轮 current-thread 每 switch 下降 4.977%、总 rq 下降 2.918%，并多推进一个 64-file 批次；但
transaction、idle、timer-rq、deadline derivation 等与 exit 无关的项也同时下降约 1%--3%，而极小
的 current-core leaf 从 187 增到 287。因此该窗口只能说明修复没有引入可见回退，不能把单轮整体
改善归因于低频 exit 路径，更不能宣称相对 dev 的性能问题已经解决。下一阶段处理高频 idle-pull
组合观察，要求 current/idle 来自同一 rq snapshot，而不是增加 idle-state mirror。

idle-pull 的 admission 随后按 Linux `idle_task(cpu)` 与 `idle_rq(rq)` 的分层完成收敛。dedicated
idle identity 是一次安装后通过 Release/Acquire 发布的固定值；“当前 owner 是否真正 idle”则必须
在同一 rq guard 内同时满足 `rq->curr == rq->idle` 与 `nr_running == 0`。不能只检查
`nr_running == 0`：CPU online 后、第一次 dispatch 前存在 `rq->curr=None`、idle identity 已发布、
`nr_running=0` 的合法过渡态，它不是可接受 pull 的 idle dispatch。

`request_idle_pull()` 在 reservation 前后各调用一次 `idle_pull_eligible()`，每次只取得一个 coherent
rq observation；publisher、reservation、claim/commit 和 target-work publication 的线性化协议保持
不变。`owner_balance_work_pending()` 与 `service_owner_balance()` 已持有 selection 产出的 `next`，
只需把它与固定 idle identity 比较，不再为 identity 分类重开 rq。旧 `CpuLocal::idle()`、
`OwnerIdleObservation`、对应 IRQ guard、qperf leaf、Starry debugfs key 和测试计数全部删除，不保留
恒零兼容字段或第二份 idle state。

确定性回归先在旧实现中观察到直接 pull 的 current/idle/runnable 为 `1/1/0`、完整 idle balance 为
`1/3/0` 并失败；修复后的 fixture 明确完成真实 idle dispatch，reservation 前后两次 recheck 最终为
current/runnable `0/2`。完整 qperf-feature 测试通过（451 unit、全部 integration、21 loom、12
doctest），ax-task 与 Starry 31 项 clippy 全部通过。

相同 x86_64/4-vCPU/1009 Hz、60 秒 ext4 marker 窗口的结构计数如下：

| qperf window | owner observation / switch | current thread / switch | current core / switch | idle / switch | runnable / switch | transaction / switch | timer rq / switch | deadline derivation / switch | 总 rq / switch | workload 进度 | host user |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|---:|
| exit explicit task current | 2.157615 | 0.558021 | 0.002287 | 1.597307 | 0 | 1.147433 | 0.843565 | 1.759575 | 4.223057 | `file-0538` | 87.197 s |
| coherent idle-pull rq | 0.566780 | 0.254772 | 0.001595 | 已删除 | 0.310413 | 1.157052 | 0.815148 | 1.731754 | 2.618166 | `file-0538` | 86.960 s |

新窗口的 owner leaf 以 `0.254772 + 0.001595 + 0.310413 = 0.566780/switch` 精确闭合；owner
observation 下降 73.731%，总 rq 下降 38.003%。transaction 上升 0.838%，timer-rq 下降 3.369%，
deadline derivation 下降 1.581%，没有证据表明被删除的 current/idle acquisition 被机械转移到
runnable leaf。两轮分别有 125,515/127,875 次 switch，均以状态 143 完成完整 timeout 窗口并推进到
`file-0538`；host user 只下降 0.272%。因此这里只确认 rq acquisition 的结构性根因已消除，不把
单轮 host 时间波动解释为吞吐改善，相对 dev 的性能问题仍未关闭。

2026-08-14 在最新 current/dev 上重新建立了同配置独立基线。current 的
`test-ext4-inode-unique` guest/QEMU 时间为 86.136/91.20 秒，最新 dev 为约 65/70.49 秒；current
慢 32.5%，超过 20% 阈值，不能继续等待完整大组掩盖问题。current 的 60.472 秒 qperf 窗口推进到
`file-0535`，host user 88.012 秒；124,526 次真实 switch 中 Blocked 77,875（62.5%）、Preempted
46,646，direct-wake activation/enqueue 与 Blocked 精确相等。相同窗口 runtime IRQ guard 进入
1,909,076 次，其中 thread-sched ticket 512,870 次、owner-rq irqsave transaction 143,833 次。
历史 dev 在相同 60 秒窗口执行 217,773 次 switch 却只消耗 72.312 秒 host user，而 current 只执行
121,350 次 switch 已消耗 88.154 秒；因此主因是每次 block/wake/switch 的固定事务成本，而不是
switch 数量增加。

第一项高频重复 owner 已按 Linux task/rq 锁层次修复。Linux
`kernel/sched/core.c::_task_rq_lock()` 只对 `p->pi_lock` 执行一次 `raw_spin_lock_irqsave()`，随后
raw-lock `rq`；`__schedule()` 同样由外层 `local_irq_disable()`/scheduler baton 提供 IRQ owner，
不会对内层 rq 重复 irqsave。旧 ax-task 的 direct wake 先取得 `ThreadSchedCell` 的 runtime IRQ
guard，再由 `OwnerRqTxn::begin()` 建立第二个 runtime IRQ guard。不能根据“IRQ 当前已关”让 runtime
返回空 token：token 允许非 LIFO 生命周期，这会让外层先释放时恢复 IRQ，而内层 raw lock 仍存活。

新 `IrqOwner<'a>` 是从外层 `IrqTicketGuard` 可变借用拆出的类型化证明；
`IrqTicketLock::lock_nested()` 返回的 raw ticket guard 同时借用该证明，因此内层 rq guard 在类型上
不能越过外层 IRQ owner。direct wake 的 Deadline source-rq 与最终 target-rq 都复用同一证明，不
保留第二套“IRQ 已关闭”推断状态。ArceOS Rust remote wait-queue 测试先等待目标真实进入 Blocked，
再通过生产 wake 路径记录 guard 所有权：旧实现确定性为 `thread_sched=1, run_queue=1` 并失败，修复
后为 `thread_sched=1, run_queue=0` 并通过。该检查点只关闭 try-to-wake-up 的重复 owner；
schedule/park/exit 的 scheduler-frame/task-lock 层次和端到端性能差距仍需继续收敛。

第二项高频重复 owner 来自 scheduler frame 内部。Linux `__schedule()` 在外层关闭 IRQ 后依次取得
task/rq raw lock；ax-task 的 rq 已通过 `OwnerRqEntry::SchedulerFrame` 复用 scheduler baton，但
park、exit、schedule、yield 仍对 task scheduler state 调用普通 irqsave lock，形成同一事务内的
第二个 runtime IRQ owner。现在 `OwnerRqEntry` 同时决定 task 与 rq 的加锁协议：普通 task context
保持 `IrqSave`，只有带 `SchedulerFrame` 类型分支的入口才能调用 raw task lock；该 unsafe 边界要求
runtime scheduler baton 覆盖两个 guard 的完整生命周期，不再新增另一套 IRQ 状态判断。

同一个 ArceOS Rust remote wait-queue 回归在 sleeper 真正进入 park 提交前挂载一次性观测。旧实现
稳定记录 `ParkIrqOwnerEntries { thread_sched: 1, run_queue: 0 }` 并失败；修复后为 `0/0`，同时 wake
事务仍保持上一检查点的 `1/0`。相同 x86_64/4-vCPU、60 秒 ext4 marker 窗口推进到同一
`file-0535`，switch 数量也接近（124,526 对 124,152）：thread-sched runtime IRQ guard 从
512,870 降至 373,137（-27.24%），全部 runtime IRQ guard 从 1,909,076 降至 1,785,929
（-6.45%），rq ticket 从 312,088 降至 250,724（-19.66%）。host user 从 88.012 秒降至
87.275 秒，仅改善 0.84%，工作负载仍未在 60 秒窗口完成；因此这里只确认 scheduler-frame
所有权重复已消除，不把局部结构下降误报为相对 dev 的端到端性能达标。

第三项重复 owner 位于物理 context-switch tail。Linux 把 `__schedule()` 已持有的 rq raw lock 与
IRQ-off 状态跨 `switch_to()` 交给新上下文，并在 `finish_task_switch()` 中完成 `prev->on_cpu` 清除后
统一 unlock/enable；tail 不重新 irqsave 获取 task/rq lock。ax-task 原先在 scheduler frame 跨架构
switch 返回后调用安全 `complete_context_switch()`，普通路径重新取得 task irqsave lock，迁移路径
还会重新取得 rq irqsave transaction。现在 tail 有独立的 unsafe scheduler-frame 入口，并复用
`OwnerRqEntry::SchedulerFrame` 同时选择 task 与 rq raw protocol；安全入口仍保留 `IrqSave`，供没有
scheduler baton 的普通 task-system 调用者使用。park、wake、switch-tail 三个一次性观测也复用同一
`IrqOwnerProbe` 状态机，不复制探针生命周期协议。

真实 ArceOS remote wait-queue 用例把目标 sleeper 的物理切换尾部加入同一检查：旧实现稳定得到
`SwitchTailIrqOwnerEntries { thread_sched: 1, run_queue: 0 }` 并失败，修复后为 `0/0`；park/wake
仍分别保持 `0/0` 与 `1/0`。相同 60 秒 ext4 marker 窗口仍推进到 `file-0535`，switch 数从
124,152 变为 125,555（+1.13%）。按每次 switch 归一后，thread-sched runtime IRQ guard 再下降
34.58%，总 runtime IRQ guard 下降 8.91%，rq ticket 下降 2.00%；host user 从 87.275 秒降至
86.524 秒（-0.86%）。端到端 workload 仍未在窗口内完成，所以这一检查点只关闭 switch-tail
重复 owner，性能阶段仍继续。

第四项重复 owner 位于 `CpuDeadlineBase` 的“先观测、后发布”两段锁。Linux 的 hrtick/clockevent
推导在 rq raw lock 与 IRQ-off 事务中形成非 timer 候选，再按 rq lock -> hrtimer base lock 的固定
顺序读取 timer head、比较并提交物理事件；调度切换中的重编程可以延后到 schedule exit，但不会为
同一次发布先后获取两次 hrtimer base lock。ax-task 原先每次 deadline derivation 先用 Observation
guard 读取 task/kernel timer head，随后再用 Publication guard 比较 publication 与 generation；两个
guard 之间的队首也不是同一个一致性快照。现在先在既有 rq/RT-period 锁序下计算非 timer 候选，
再以一个 Publication guard 同时读取 timer head、比较旧 publication、提交 generation，保持
rq/RT-period -> deadline-base 顺序且只保留一个权威事务。

真实 ArceOS timeout wait 在一次本地发布上记录 deadline-base 入口：旧实现确定性得到
`DeadlinePublicationEntries { observation: 1, publication: 1 }` 并失败，新实现为 `0/1`。相同
x86_64/4-vCPU、60 秒 ext4 marker 窗口仍到 `file-0535`，switch 数从 125,555 变为 124,000
（-1.24%）。`CpuDeadlineBase` 总 guard 从 617,564 降至 440,735（-28.63%），其中 Observation
从 206,933 降至 33,459，Publication 与 derivation 保持一一对应；全部 runtime IRQ guard 从
1,645,178 降至 1,458,352（-11.36%）。本轮 host user 为 95.305 秒，较上一轮 86.524 秒增加
10.15%，且工作负载仍未完成，因此只确认重复 base lock 被结构性消除，不声称端到端改善；新的
高频项是独立的 root RT-period owner（235,405 次），应继续按 Linux RT bandwidth 生命周期处理。

AxVM 不存在另一套 host 物理 timer：AArch64/LoongArch vCPU 与设备定时任务已经经 ax-task kernel
timer 队列，最终统一交给 ax-runtime `LocalClockEvent` 编程。客户机虚拟 counter/PPI、generation
和注入状态属于 guest architecture state，语义上不能并入 host scheduler deadline；可复用的是现有
host timer transport，而不是删除客户机自己的状态机。Linux RT bandwidth period timer 同样是独立
的 replenishment source，不能为了减少锁次数与 hrtick/普通 kernel timer 合并。

第五项重复 owner 是共享物理 clockevent 对 root RT period expiry 的轮询。Linux 的
`struct rt_bandwidth` 以 `rt_period_timer` 自身保存下一次 expiry；`do_start_rt_bandwidth()` 只在
激活状态机时取得 `rt_runtime_lock`，普通 scheduler decision 不为了合并硬件事件读取它。timer
callback 在锁内 `hrtimer_forward_now()` 推进 expiry，再释放 root lock 扫描并补充各 rq。ax-task
此前把 root period 合并进唯一 `LocalClockEvent` 时，每次 deadline derivation 都调用
`RootRtBandwidth::deadline_for()` 重新取得 period state lock；这不是另一套 timer，却把 Linux
hrtimer 已发布的 expiry 错误退化为高频共享状态查询。

现在唯一 `RootRtBandwidth` 在 activate、begin-period 推进、owner migration 和停止四个权威状态
转换中同步发布 per-CPU expiry 投影，generic scheduler deadline 只 Acquire 读取本 CPU 投影。状态锁
仍唯一拥有 owner、generation、firing 和 deadline；投影只承担 Linux hrtimer expiry 对物理
clockevent transport 的输入职责。迁移先发布 replacement 再撤销 offline CPU，因此并发 derivation
至多保留一次无害的提前事件，不会漏掉 active period。begin/finish 分段锁、firing 期间的 activation
记账和各 rq runtime ledger 均未合并或旁路。

ArceOS `task-rt-policy` 在 CPU1 保持真实 FIFO worker 令 root period active，同时让 CPU0 注册真实
sleep deadline：旧实现确定性得到
`DeadlinePublicationEntries { observation: 0, rt_period_observation: 1, publication: 1 }` 并失败，
修复后为 `0/0/1`。相同 x86_64/4-vCPU、60 秒 ext4 marker qperf 窗口仍推进到 `file-0535`，switch
从 124,000 变为 126,191（+1.77%）；root RT-period ticket 从 235,405 降至 20,330（-91.36%），
全部 runtime IRQ guard 从 1,458,352 降至 1,289,114（-11.60%），host user 从 95.305 秒降至
85.881 秒（-9.89%）。deadline derivation 增加 3.57%，对应 `CpuDeadlineBase` guard 增加 4.15%，
说明 period 成本没有被转移到隐藏的第二套锁；workload 仍未完成，不能据此宣称已达到 dev 的最终
端到端性能目标。

CI run `31729428661` 的唯一真实失败进一步暴露了同一 owner publication 边界：running Fair task
切换为 FIFO 时，事务可以同时返回 `preempts_current` 和 `rt_period_started`。旧 `else if` 只发布
remote reschedule，吞掉新激活 period 的 `REQUEST_OWNER_WORK`；物理 IPI 可以合并，但两个逻辑请求
不能互相替代。Linux 的 `start_rt_bandwidth()` 同样独立于 class dispatch/preemption 通知。ArceOS
target-side probe 在旧实现确定性观察到事务要求 `reschedule=true, owner_work=true`，实际只交付
`true/false`；改为两个独立 publication 后为 `true/true`。完整 RISC-V Rust 测例序列还验证了另一种
合法状态：前序 RT runtime 尚待下一 period callback 清空时，事务与交付均为 `true/false`。这与
Linux `do_sched_rt_period_timer()` 在无 runnable 但仍有 `rt_time` 时继续 period 的语义一致，不应靠
强制停表或测试延时掩盖。定向 `task-rt-policy` 和 CI 对应的 `all` 序列均已通过。

第六项重复 owner 位于 timed park 的 hrtimer 注册/取消。Linux
`kernel/time/hrtimer.c:1471-1495` 在一次 `lock_hrtimer_base()` 事务内完成 enqueue 与需要时的
`hrtimer_reprogram()`；取消同样在一次 base lock 内完成 `remove_hrtimer()`
（`kernel/time/hrtimer.c:1509-1534`）。ax-task 此前先以 Registration guard 修改 task deadline
queue，释放后重新取得 rq observation，最后再以 Publication guard 读取同一个 timer head 并提交
物理 deadline。一次 park arm/cancel 因而为同一个 base 状态取得两把 IRQ-save 锁；取消失败回滚还
会第三次重进 base，并遗留了只服务该分段事务的 publication invalidation 路径。

现在先按既有 rq/RT-period 语义形成非 timer 候选，随后用唯一 Registration guard 完成 queue
arm/cancel、读取新 timer head、比较旧 publication 和提交 generation。generation exhaustion 在同一
guard 内回滚 queue/core ownership，不再释放后重锁或失效整个 publication。kernel timer 的本地
register/cancel 也复用同一 Registration guard；失败时在原 guard 内恢复 entry。远端 cancellation 仍保留
Linux 的保守 stale edge 语义，不跨 CPU 重编程 clockevent。owner safe point 用 sequence 保护的一份
`CpuDeadlineSnapshot` 同时观察 timer head 与已发布 deadline；它只是权威 base 的一致只读投影，任何
不匹配都回到 base lock 复核，不再把两个不同时刻的 atomic 当成两个事实来源。

真实 ArceOS `task-wait-queue-remote-wake` 的本地 timeout 先建立确定性红测：旧实现一次 timed park
稳定记录 `registration=1, publication=1` 并失败，修复后为 `1/0`；随后在 probe arm 后强制插入一次
无关 owner pass，复现 CI 分组执行时 probe 被提前完成、timed park 反而记录 `registration=0` 的失败。
probe 现在只在 `arm_current_park_deadline()` 的事务入口开始计数，异常退出由 RAII 恢复 probe 生命周期，
因此测量的是目标 transaction，而不是同 CPU 上一段不确定时间窗口。`task-rt-policy` 同时确认复用
registration base 后仍以无锁投影读取 active root RT-period expiry；clockevent soft expiry 则在一个
SoftExpiry guard 内按同一 budget 推进 task deadline 与 kernel timer。两项 x86_64 QEMU 用例与 ax-task
六组 clippy 均通过。与上一检查点完全相同的 x86_64/Q35/4-vCPU/99 Hz、60 秒 ext4 marker 窗口均推进
到 `file-0535`；switch 为 126,191/127,943。按 switch 归一后，deadline-base guard 从
3.637716 降到 2.969869（-18.36%），独立 Publication guard 从 1.773716 降到 1.256739
（-29.15%），全部 runtime IRQ guard 从 10.215578 降到 9.332093（-8.65%）。host user 从
85.881 秒降到 85.524 秒（-0.42%）；相对 dev 的端到端差距仍超过 20%，所以该检查点只关闭 timer
base 重复所有权，下一步继续检查 current-handle 与 block/wake 固定事务。

第七项重复 owner 是同一个 rq 事务把 preemption 与 owner-work 拆成两个 scheduler-request
generation。此前修复 `else if` 时已经保证两个逻辑原因都不丢，但 running task 晋升 FIFO 和 direct
wake 仍先发布 `REQUEST_PREEMPT`、完成一次 delivery，再单独发布 `REQUEST_OWNER_WORK`。这允许本地
scheduler safe point 或远端 IPI 在两次 publication 之间消费第一代；即使 generation 状态机最终不
丢 work，也会重复进入 owner-delivery/IRQ scope，并可能发送两次物理门铃。Linux 在 rq transaction
完成后先让 resched 与 deferred-work state 全部可见，再以同一 IPI/irq_work edge 通知 owner。

现在 `CpuRemote::request_remote_reschedule_with_scheduler_work()` 在一个 owner-delivery lease 和一个
IRQ scope 内把 `REQUEST_PREEMPT | REQUEST_OWNER_WORK` 发布为同一 generation，随后最多发送一次物理
doorbell。policy transaction 对四种结果显式分派；direct wake 也把 RT/DL push 与新激活 RT period
折叠成一个 owner-work reason，且在同时要求抢占时复用同一组合入口。逻辑 bit 仍分别由 scheduler
claim/drain 消费，没有把 reschedule 与 period/push 语义互相替代。

真实 ArceOS `task-rt-policy` 为一次同时需要 reschedule 与新 RT period 的策略晋升统计 request
publication batch：旧实现 LoongArch64 精确用例稳定得到 2 并失败，新实现为 1 并通过；原有断言仍
分别验证两个 logical delivery 均已交付，heartbeat 仍验证 FIFO task 能跨 period 继续运行。因此这里
没有延长 timeout 或放宽 RT replenishment 结果，而是直接约束 Linux 对应的发布顺序与物理边数量。

第八项重复 owner 是 direct wake 为只读调度决策复制完整 `SchedulingEntity`。Linux 的
`try_to_wake_up()` 在 task/rq 锁约束内把同一个 `task_struct` 依次交给 `select_task_rq()`、
`enqueue_task()` 和 `check_preempt_curr()`；Fair/RT/Deadline entity 始终只有 task 或 rq 一个可变
owner。旧 Rust 边界却让 `select_priority_cpu()` 和 `wakeup_preempt()` 按值接收 entity，一次真实
remote wake 因而在 placement 与 preemption 两处各复制一次；Deadline 分支会连 CBS runtime、absolute
deadline 和 server 状态一起深拷贝。

现在 placement、cpupri/cpudl 与 class preemption 的完整调用链都接收 `&SchedulingEntity`。wake
transaction 仍在 task lock 下处理 Deadline activation，随后用 `take_active()` 把唯一可变状态 move
到 owner rq；借用不会跨越 task/rq guard，也没有改成 `Arc` 或共享可变 entity。相同借用边界也覆盖
affinity reconciliation、initial delivery、Deadline replenishment 和 owner enqueue，避免保留第二套
只为 direct wake 特判的 API。

真实 RISC-V ArceOS remote wait-queue wake 对 placement/preemption 两个只读访问计数：旧实现稳定为
`reads=2, copies=2` 并失败，新实现为 `reads=2, copies=0` 并通过；同一用例继续验证跨 CPU wake、
task-IPI 进度、park/switch/wake IRQ baton 与 deadline soft expiry。因此该回归约束的是单一 entity
所有权，而不是通过缩短 workload 或放宽唤醒时限获得绿测。

第九项重复 owner 是 rq current 的只读比较仍隐式构造 snapshot。RT/Deadline current 的权威 entity
留在 class node，Fair/stop current 的权威 entity 留在 `CurrentDispatch`；旧
`current_scheduling_entity()` 为统一两种物理存储直接返回 owned value，所以即使 wakee 已改为借用，
`wakeup_preempt()` 仍会在每次 non-idle 比较复制一次 current，Deadline 同样复制完整 CBS 状态。

现在 `linked_current_entity()`、`CurrentDispatch::owned_scheduling_entity_ref()` 和
`current_scheduling_entity()` 形成一条权威引用链；runtime deadline derivation、Fair virtual-time 读取与
wake preemption 都在 rq guard 内直接借用。确实要跨越 rq mutation 或作为返回快照的路径显式调用
`.cloned()`；`QueuedThread::entity()` 也改为借用，只保留命名明确的 `entity_snapshot()`，没有并存
`entity()`/`entity_ref()` 两套相似 API。

真实 RISC-V ArceOS remote wake 在目标 CPU 上保持一个普通 Fair current，强制经过 non-idle
preemption 比较。旧实现稳定为 `reads=3, copies=1` 并失败，新实现为 `reads=3, copies=0` 并通过；
occupier 与 sleeper 的 handle 都在 readiness 检查前进入 RAII owner，readiness 等待有明确上限；正常路径
显式 stop/wake/join，断言失败或 panic 路径由析构执行同样的释放。因此该回归不会把线程、wait queue 或
rq 状态泄漏给分组后续测例，也不会用无界等待掩盖调度失败。

## 模块化结果

- `TaskSystem` orchestration 只负责编排，registry/reap、placement、owner scheduling、deadline、PI、balance、deferred work 分模块；
- `CpuRunQueueState` 按 current/class queue、deadline/RT bandwidth、clock 与派生 publication
  划分；`CpuLocal` 只保留 switch-tail、drain scratch 和 owner continuation，facade 实现按
  owner dispatch、hard scheduler deadline、ktimer worker handoff、idle polling 分文件；
- `TaskRuntime` 按 move-only resource/context ABI、scheduler/monotonic clock ABI 与 provider
  interface 分文件，仍保持单一 trait-FFI 边界；
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
- #1838：Starry USBFS no-std `event-listener` wake transport 可能在长序列 USB audio 中永久自旋；
- #1877：Axvisor NUC guest smoke 完成后宿主延迟触发 `#PF`，需单独追踪设备/IRQ teardown 生命周期；
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
