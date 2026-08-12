# ax-task 任务期限与 ax-runtime 物理时钟事件

## 文档状态

本文定义 ax-task 迁移后的 timer、hard IRQ、CPU-local 和物理 clockevent 所有权边界。它正式替代“`components/ax-task` 必须与 PR #1596 字节一致”的旧要求。

设计同时覆盖任务调度期限和内核 task-context callback timer。两者共享同一个 per-CPU
deadline owner、物理 clockevent 和 `ktimers/<cpu>` worker，但使用相互独立的 typed clock
base；任意 callback 不进入任务期限 heap，也不在 hard IRQ 中执行。

## 问题

旧集成分别维护三种“下一个 timer”：

- scheduler 的下一任务期限；
- runtime 的 periodic tick；
- 最近一次写入物理设备的 deadline。

三份标量独立更新会产生以下问题：

- cancel 或替换成更晚期限后，硬件仍保持旧 arm；
- `Firing` 期间的新 earlier deadline 被旧 completion 覆盖；
- rearm 只留下 tombstone，容量取决于历史 arm 次数；
- 物理 timer IRQ 直接调用任意 consumer callback；
- `u64::MAX` 被当作“无期限”下发设备，转换时溢出；
- 丢失或过晚的硬件边可能永久挂起 sleeper。

## Linux v7.1 参考

参考提交为 `8cd9520d35a6c38db6567e97dd93b1f11f185dc6`，配置启用 `PREEMPT_RT`、`HIGH_RES_TIMERS`、SMP 和 CPU hotplug。

- `kernel/time/clockevents.c::clockevents_program_event()` 是物理事件编程边界；
- `kernel/time/hrtimer.c::hrtimer_interrupt()` 先失效已触发的 event，再处理有界 hard timer，并统一计算下一 event；
- `kernel/time/tick-sched.c::tick_nohz_stop_tick()`、`tick_nohz_idle_enter()` 和
  `tick_nohz_idle_exit()` 把 idle/nohz 状态与物理 tick 编程收敛到 per-CPU owner；
- PREEMPT_RT 只把非 hard hrtimer callback 移到 soft/threaded 上下文，显式 hard timer 仍可在硬中断执行；
- scheduler placement/migration 由 owner runqueue 保护；
- `irq_work` 与 scheduler IPI 遵守“先发布 work，后发送门铃；handler 先 claim 旧门铃”的顺序。
- `net/core/dev.c::__napi_schedule()`、`napi_schedule_prep()`、`napi_poll()` 和
  `net_rx_action()` 用一个 owner bit 加 sticky missed publication 保证协议 poll 只有一个
  consumer；hard IRQ 只发布 work，budget drain 发生在后续执行上下文。
- KVM 不创建自己的 per-CPU timer worker。AArch64
  `arch/arm64/kvm/arch_timer.c::soft_timer_start()`、RISC-V
  `arch/riscv/kvm/vcpu_timer.c` 和 x86 LAPIC timer 都把每 vCPU 的逻辑期限注册到宿主
  hrtimer；vCPU block 仍由通用 KVM wait/scheduler owner 执行。显式
  `HRTIMER_MODE_ABS_HARD` callback 可以在 PREEMPT_RT hard IRQ 中直接发布 IRQ/wake，
  但 TGOSKits 的 VM callback 会取得 task-context lock 和通知 wait queue，因此改由现有
  `ktimers/<cpu>` 执行，而不是复制 KVM 的 hard-callback 上下文。

TGOSKits 采用相同的所有权与排序，不复制 Linux callback 形态：ax-task 发布调度期限，ax-runtime 独占物理 clockevent。

### runqueue clock 与物理时间域

Linux v7.1 的 `struct rq` 自己保存 `clock`/`clock_task`，`update_rq_clock()` 要求持有目标
`rq->lock`，并在一次 rq 事务内用 `RQCF_UPDATED` 阻止重复更新；后续调度类只读已经接受的
`rq_clock()` 快照。TGOSKits 采用相同所有权：

- `CpuRunQueueState::RunQueueClock` 是 scheduler 时间的唯一状态源，只能在目标 runqueue 的
  IRQ-safe raw lock 内更新；
- `TaskRuntime::rq_clock_sample(cpu)` 只提供指定 CPU 的已校正 scheduler clock 和累计 hardirq
  时间，不直接成为 Deadline、RT 或 Fair 的时间真值；远程 wake 锁定目标 rq 后，也必须读取
  目标 CPU 的 sample；
- 第一个样本建立基线，随后按 Linux 的 signed-delta 回绕顺序累计；负向 source 抖动不允许
  把 rq clock 倒退；
- 一个 owner rq 事务最多更新一次。dispatch settle、timer 重编程和 switch plan 等事务尾只能
  读取 `RunQueueClockSnapshot`，不得再次读取 source；
- ax-runtime 的 common IRQ entry/exit 独占每 CPU、嵌套安全的累计 hardirq 时间；rq 保存
  `clock/clock_task/prev_irq_time`，只在同一个 rq transaction 中扣除新增 hardirq 时间。
  当前没有可独立证明的 steal-time source，因此只扣 hardirq，不伪造 steal-time。

物理 timer 仍使用 `MonotonicInstant/MonotonicDeadline`。周期 Fair balance 对应 Linux 的
`jiffies`/`sd->last_balance`，因此也属于 monotonic cadence：clockevent 到期只把它提升为
sticky owner work，scheduler 在任务上下文消费并从完成时刻重置或 backoff。idle/newidle
balance 是独立事件，不受周期 deadline gate 限制。任何测试若要让 CBS、zero-lag、sleep 或
periodic balance 的物理事件到期，必须显式推进 monotonic fake；`*_at(now)` 只推进 rq source，
不能重新把两个时间域耦合成测试兼容路径。

## ax-task 的所有权

每个 CPU 的 `CpuRemote` 持有一个 IRQ-safe、固定容量的 `TaskDeadlineQueue` base。它是该 CPU
task deadline 的唯一 owner：本地 timer IRQ/soft worker 负责消费，远程 `task_rq` 迁移只在
持有对应 rq/thread 事务时取消或转移旧 base 条目。条目必须是 generation-bearing 的值记录，只允许：

- sleep、park、wait timeout；
- RR、Fair、Deadline/CBS/GRUB 的调度期限；
- ax-task 自身为推进 deferred task-work 所需的期限。

条目禁止保存：

- 任意闭包或函数 callback；
- OS/进程/驱动对象；
- 未受 generation 保护的裸指针；
- 需要在 IRQ 中析构的所有权。

每个 embedded timer node 最多有一个 active heap entry。rearm 物理替换旧项，cancel 物理移除旧项，不以 tombstone 消耗容量。

### 内核 callback clock base

通用内核 callback 使用独立的 `KernelTimerQueue`，与 typed `TaskDeadlineQueue` 一起存放在
同一个 `CpuDeadlineState` 中。它不是 ax-task 调度状态，也不得复用
`TaskDeadlineKind`；共享的是 per-CPU owner、IRQ-safe deadline lock、物理最早期限选择和
`ktimers/<cpu>` single-consumer 协议。

registration 在调用任务上下文分配完整 entry 和 callback ownership，随后绑定当前 owner
CPU。entry 状态只允许：

```text
Queued -> Expired -> Executing -> Completed
   |          |
   +----------+-----------> Cancelled
```

- `Queued` entry 按 `(deadline, sequence)` 排序；相同期限保持 FIFO；
- hard IRQ 只把 due entry 从 active queue 移入 expired FIFO，并发布 sticky ktimer work；
  移动期间不分配、不 free、不执行 callback；
- worker 在 deadline lock 内 claim 一个 `Expired -> Executing` entry，释放 IRQ guard 和所有
  scheduler/deadline lock 后才调用 callback，完成后进入 `Completed` 并释放 entry；
- cancel 与 expiry/claim 在 owner deadline lock 下决定唯一 winner。成功取消 active 或
  expired entry 后，ownership 移出 lock 再析构；已经 `Executing` 或 terminal 时返回明确的
  non-cancelled 结果，不等待 callback；
- handle 保存 owner CPU 和不可复用的 identity/generation。跨 CPU cancel 可以锁定原 owner
  base，但只在当前 owner CPU 取消时重新发布物理 deadline；remote cancel 允许保留一个
  conservative stale hardware edge，由 firing transaction 重新计算，不跨 CPU 修改物理
  comparator；
- CPU offline 必须同时确认 active queue、expired FIFO、in-flight callback 和 ktimer sticky
  publication 全部 quiescent。

callback API 只暴露绝对 monotonic deadline、typed handle、cancel outcome 和
`FnOnce(MonotonicInstant) + Send + 'static`。不得暴露 `LocalClockEvent`、硬件 comparator、
deadline-base lock 或 owner CPU 裸指针。注册失败必须返回可匹配的 capacity/generation/context
错误，不能静默退回 AxVM 私有 worker。

### ParkTicket

`ParkTicket` move-own 一个 park generation 及其可选 deadline token。取消流程先校验 owner CPU、thread generation 和 token，不提前消费 ticket。owner mismatch 或可重试失败必须保持 ticket 与 heap entry 原样。

只有两种情况可以清除 ticket：

1. 精确匹配的 heap entry 已删除；
2. expiry path 已经取走同一 generation，scheduler safe point 将按 generation 决定 timeout 是否胜出。

notify 与 timeout 只能有一个 winner。

### SchedulerDeadlineUpdate

本 CPU 最早任务期限改变后，ax-task 发布：

- 单调递增且非零的 generation；
- `Option<MonotonicDeadline>`；

deadline 语义未改变时，owner 保留原 generation，不制造一次新的物理发布；期限改变时才
递增 generation。deferred task-work 使用独立的 scheduler request reason，不再复制到
clockevent publication。runtime 只能丢弃旧 generation，不得以相同 deadline 值推断
publication 已被处理。

## ax-runtime 的所有权

每个 CPU 只有一个 `LocalClockEvent`，只能在 `ExclusiveCpu` 覆盖本地 IRQ/re-entry 排除时修改。状态机携带单调 CPU lifecycle epoch：

```text
Offline
  | online
  v
Idle <-------------------------+
  | arm                         |
  v                             |
Armed(deadline) -> Firing ------+
       IRQ          finish/stop
```

- `Offline`：CPU area 存在，但物理事件不可用；
- `Idle`：runtime 不认为设备有有效 arm；
- `Armed`：一个绝对 deadline 已写入设备；
- `Firing`：旧 arm 已失效，handler 正在合并更新。

online 与 offline 都推进 epoch。进入 `Firing` 会产生不可复制的
`ClockEventFiringToken`；finish 必须消费该 token。若 CPU 已经过一次
offline/re-online，旧 token 只能失效，不能提交到新周期。

`LocalClockEvent` 是以下状态的唯一存储：scheduler generation、scheduler deadline、
periodic deadline 和当前物理 arm。禁止旁路 scalar cache。

### 重新编程规则

在 `Armed` 状态：

- selected minimum 变早：重编程；
- selected minimum 变晚：同样重编程；
- 删除最后来源：stop device，进入 `Idle`；
- 语义状态完全相同：不写设备。

`Firing` 期间只更新逻辑 source state。handler 结束时从最新 task deadline 和 periodic deadline 计算一次 authoritative minimum，并且只提交一次硬件动作。

Deadline CBS/zero-lag hard timer 的对象生命周期由 owner rq 的 Deadline member set 保持，
等价于 Linux 把 hrtimer 嵌入 `sched_dl_entity`。hard IRQ 只用 generation-bearing event 在该 rq
取得 `ThreadCore` lifetime anchor，释放 rq 后再按 `p->pi_lock -> rq` 顺序执行 CBS 事务；禁止
通过 task-only 全局 registry 升级 `ThreadId`，也禁止把裸 timer-node 指针重新放回 heap。

物理 IRQ 先执行 claim。这里必须区分“逻辑上没有可消费的 arm”和“物理 source 已经
静默”：

- `Offline/Idle/Firing` 收到的 stale/spurious edge 不进入 ax-task，但返回前必须 stop/mask
  物理 clockevent；否则 level/pending source 可以在 EOI 后立刻重入，形成 IRQ storm；
- `Armed` 收到物理 edge 时一律执行 `Armed -> Firing(token)` 并失效旧 arm，和 Linux
  `hrtimer_interrupt()` 相同；若 edge 早于逻辑期限，有界到期扫描自然不产生 due work，
  finish 再按最新 source state 统一重编程一次。不得在 claim 前自行判断 early edge 并走
  第二套 rearm 路径。

stop/mask 不是中断控制器 ACK/EOI。x86 LAPIC timer mask、AArch64 generic timer disable、
RISC-V compare 更新和 LoongArch timer disable/clear 由 clockevent backend 实现；控制器的
claim/ACK/EOI 仍由 trap/IRQ 入口成对完成。若 `Firing` 状态遇到被屏蔽的嵌套旧边，外层
firing transaction 会在 finish 时根据最新 source state 重新 program。

### 上下线

early platform 初始化把 source 放在 masked、non-firing 状态。online 顺序为 program finite deadline，再 unmask。offline 先 mask/stop 物理源，再允许 scheduler 发布最终 `Offline`。re-online 重新执行 program-before-unmask。

CPU 数上限统一使用：

```text
min(platform_cpu_count, CPU_CAPACITY)
```

### 时间转换

- ax-task 的 heap 保存物理 monotonic task deadline；CBS、RT/Fair 调度边界保存 rq-clock
  deadline。两者只能通过“目标 rq 已接受的 snapshot + 同一事务的 monotonic sample”按正向
  delta 映射，不能直接比较绝对值；
- ax-task 可以发布已经到期的精确值，scheduler safe point 负责正确性推进；
- 无期限使用 `None`；
- ns 到 tick 向上取整；
- 已过期或 sub-tick deadline 钳制到设备最小非零 delta；
- 超出设备参数宽度时饱和；
- 架构 absolute-counter/alignment 运算前先完成饱和，禁止回绕为早期时间。

后三项只发生在 ax-runtime/platform 的物理编程边界。禁止把设备 resolution 反向传入
ax-task 并平移逻辑期限；否则 scheduler 与用户 absolute sleep 会永久多出一个架构相关的
固定尾延迟。

## hard IRQ 顺序

平台控制器和 timer device 在 runtime handler 前 claim/ACK 或失效 delivered event。runtime 顺序固定为：

1. claim 当前 arm；无逻辑 owner 的 stale edge 先 stop/mask；任何有效 `Armed` edge 都进入
   `Firing(token)` 并忘记旧 arm；
2. 推进 periodic source；
3. 调用 ax-task 的 bounded `on_clock_event(now, budget)`；
4. 发布 reschedule 与 deadline/deferred-work sticky state；
5. 合并 handler 期间所有 source update；
6. 统一 program 或 stop 一次；
7. 返回平台完成 EOI。

步骤 3 到 6 都受 firing token 的 CPU epoch 约束；旧 token 的 finish 不得发布
逻辑 source 或物理动作。

hard IRQ 必须：

- 无分配、无 free；
- 无睡眠和无等待外部 owner；
- 工作量受 budget 限制；
- 不执行任意 callback；
- 不持有 Starry、驱动或进程对象裸指针。

过期 task deadline 只复制到预分配 CPU-local buffer。真正的 thread wake 只由固定绑定的
`ktimers/<cpu>` FIFO worker 在任务上下文执行；Deadline extension callback 和资源回收仍由
独立 task-work worker 执行。scheduler safe point 不消费 task timeout，外部调用方也没有
公开 drain buffer 的入口；buffer 只属于 ktimer worker 的单 consumer 生命周期。

### batch 耗尽

hard scheduler timer 预算耗尽时发布 sticky scheduler request 和 `need_resched`，owner safe
point 在 drain 前 claim 旧 publication；task timeout 预算耗尽时只发布 ktimer event，worker
在 drain 前 claim 旧 publication。两者若仍有 remainder 或并发新 publication，都重新发布
自己的 generation-bearing sticky work；旧 completion 不得清掉新工作。

### 无轮询恢复路径

物理 clockevent 是 deadline 推进的正式所有者，不是可丢失后再由 scheduler 偶然扫描补救的
加速路径。实现不提供 `claim_due`、`recover_overdue`、析构恢复或周期轮询。正确性由
generation publication、`Firing` 合并、idle 最终复查和远程 doorbell 共同保证；task timeout
预算耗尽只发布有界 soft-timer work，由 `ktimers/<cpu>` 这一 threaded-softirq 等价路径继续
消费；hard scheduler timer remainder 由 scheduler request 驱动 owner safe point。两条路径
都不能依赖偶然 tick 推进。

## idle 与远程投递

idle 在 IRQ 关闭状态完成：

1. 发布 polling；
2. 检查 remote/task deadline pending；
3. 同步 deadline generation；
4. 清 polling 并做最终 recheck；
5. 进入架构原子 wait：x86 `sti; hlt` 或其他架构 WFI/idle region。

pending work 禁止睡眠。若 `Armed` 已过期，idle 可精确 claim 为 `Firing` 并执行同一 bounded accounting transaction，但不增加物理 IRQ 计数。

远程 producer 顺序：

```text
publish payload -> Release sticky/epoch -> send IPI
```

handler 入口先消费旧 doorbell，再 drain payload。并发 producer 看到旧 doorbell 已被 claim 后，可以创建新物理边。

已合入 dev 的 `ax-ipi::DeliveryEdge` 是唯一物理 coalescer。ax-task 只维护 owner-rq 的逻辑
request/ack generation；ax-runtime 在 IPI handler 入口统一 claim 物理 edge，随后读取 scheduler、
hard-call 等各自的逻辑 pending 状态，不保留 scheduler 专用 claimed epoch。

## 网络 poll 的单 owner 交接

网络协议栈采用与 Linux NAPI `SCHED/MISSED` 相同的所有权形态，但不复制 softirq API：

1. socket、设备 worker 和控制路径只推进 wrapping request generation；
2. `scheduled` 从 false 变为 true 的 producer 唤醒唯一永久 worker；
3. worker 是 smoltcp poll 的唯一 owner，任务调用者不能 opportunistic 接管；
4. worker 完成 snapshot generation 后先发布 completion，再释放 owner bit；
5. 释放前后的并发 request 分别由 generation recheck 或新的 false-to-true wake 捕获；
6. 同步 egress flush 等待 completion generation，不持有协议锁忙等或反复 yield。

`SERVICE`、socket set、设备协议对象属于纯任务上下文，使用 PiMutex；hard IRQ 只能通过
`IrqWaitCell`/sticky event 交接。VirtIO transport 的寄存器/virtqueue raw gate 仍是极窄、
不可睡眠的硬件临界区，task 侧持 gate 时关闭本地 IRQ，IRQ 侧只执行有界 ACK/status 操作。

## IRQ endpoint 独立生命周期

timer/IPI/UART/perf 等 IRQ-visible 数据应与上层对象拆开：

```text
Unpublished -> Published -> Draining -> Dead
```

撤销时先关 producer admission 和推进 generation，再 mask/ACK 物理源，最后等待本地 IRQ reader/owner-CPU grace。只有进入 `Dead` 后，task worker、wake target、ring 或 OS extension 才能释放。

IRQ endpoint 只保存固定值状态和稳定 registration；任务态对象通过 `Arc`、generation token 或 move-only lease 保活。不得以“外部通常不会同时销毁”为安全条件。

`IrqWaitCell` 的 registration 使用 `Detached -> Attached -> Notifying -> Draining -> Detached`。IRQ 完成 direct wake 后只进入 `Draining`，不立即开放同地址节点复用；任务侧 move-only `IrqWaitToken` 先撤销 publication，转换为 `IrqWaitDrain`，再在 notifier grace 完成后通过 `try_finish()` 开放复用。registration 最终 Drop 对应 `Dead`。正常 API 路径不泄漏，hard IRQ 不等待也不析构；只有调用者显式遗忘 token、违反回收协议时，Drop 才以泄漏代替 UAF，这是 Rust 允许的失效安全兜底，不是正常生命周期的一部分。

## 通用 timer 消费者

内核 task-context callback timer 进入独立的 kernel callback clock base；POSIX/wall timer
仍保留各自的进程语义和 queue metadata，只把需要宿主 monotonic 唤醒的最早期限接到该
能力边界。

### AxVM

AxVM 不再拥有 `TimerQueue`、token owner map、`IrqNotification` 或 `axvm-timer-*` worker。
设备 timer、x86 LAPIC/PIT、LoongArch guest timer 和 AArch64 blocked-vCPU timer 都注册到
宿主 kernel callback clock base；callback 仍只在线程上下文执行。

AxVM 保留虚拟化语义自身的状态：每 vCPU CVAL/CTL、AArch64
`Aarch64TimerBinding::wait_generation`、host CNTV PPI activation owner，以及架构 backend
要求的 cancel token。kernel timer handle 决定宿主 entry 的 at-most-once ownership；架构
generation 决定一个已经开始执行的旧 wake hint 是否仍对当前 vCPU wait 有效。两者不能合并
成一个 bool，也不能让 callback 直接断言 guest PPI。

vCPU 迁移不修改已注册 entry 的 owner 字段。旧 wait 先 invalidate generation 并 cancel 原
handle，新 owner 按最新 guest deadline 重新注册；AArch64 CPU-local host timer activation 仍在
原 pCPU 完成 deactivate。VM teardown 必须取消所有保存的 handle，fire-only device timer 的
callback 只持有可失败升级的 VM identity，不能让 timer entry 延长整个 VM 生命周期。

### Starry

Starry wall/POSIX timer 的 queue metadata 使用 PiMutex。producer 先修改队列并推进 epoch，再通知固定 worker。worker 在取 snapshot 前采样 epoch，使并发 registration 进入 wait predicate，而不是被当作旧 baseline 吸收。

只有 IRQ-facing notification endpoint 使用原子和 generation-bearing wake。

若未来出现多个通用消费者，应新增独立 timer component，不扩宽 ax-task 的任务期限接口。

## UART 同步边界

serial 暴露三种 capability：

- task/control endpoint：配置和普通数据流；
- hard-IRQ endpoint：有界 status、ACK/mask、FIFO drain、event publish；
- emergency-TX endpoint：panic-safe、非阻塞寄存器访问。

worker 独占 normal port。任务态 control、completion、subscription 可以使用 sleepable lock。IRQ、scheduler、panic、atomic log 只能使用固定队列、原子、`IrqWaitCell` 或 non-blocking raw gate。

普通 TX 的固定容量 MPSC ring 区分 reservation 与 publication，consumer 不等待被抢占的 producer。start/stop epoch 拒绝旧设备生命周期的 frame。register gate 竞争只设置 sticky retry 并唤醒 worker，不能伪装为 IRQ 已完成。

panic TX 有固定字节预算，竞争时丢弃。它与 IRQ endpoint 共用同一 non-blocking register gate，不能等待被当前 IRQ 打断的寄存器 owner。

## 切换与资源安全

- CPU-local 可变对象要求 `CpuPin + ExclusiveCpu`；
- scheduler baton 是跨 context switch 的唯一 guard；
- switch tail 先清 outgoing `on_cpu`，再允许迁移和资源回收；
- stack、TLS、context、address space、extension 由事务式 builder 构造，失败逆序回滚；
- Starry clone 在 scheduler stage 成功后才发布 PID/TID，公开 identity commit 后只允许 infallible activate；
- exec 先安装新 page-table root，再延迟释放旧 address space；
- x86 double fault 使用专用 per-CPU IST，不复用可能损坏的任务栈。

## 验证

确定性 virtual runtime 覆盖：

- earlier/later/cancel/rearm；
- stale generation 与 `Firing` 期间 update；
- final arm removal；
- batch exhaustion 和 remainder rearm；
- remote deadline 与 idle lost-wakeup；
- hard IRQ batch 耗尽后的 owner safe-point 有界 continuation；
- park notify-vs-timeout 唯一 winner；
- owner mismatch cancellation retry；
- IPI consume/publish 生命周期；
- switch-tail 顺序与 raw switch 前的失败注入回滚；
- CPU offline/re-online；
- IRQ endpoint revoke/quiesce/reclaim。

loom 覆盖 generation publication、publish-before-IPI、park 唯一 winner、`IrqWaitCell` notify/drain、同地址 pointer ABA 和 doorbell claim race。

UART 测试覆盖 hard IRQ 无分配/无阻塞、有界 drain、overflow、worker wake race、`try_write` 与 emergency/normal TX 互斥。

目标 crate test/clippy 后，串行运行四架构 ArceOS 与 Starry QEMU。只接受正式 success regex。hang 用 GDB 检查 timer begin/finish、IPI consume、idle commit、switch tail 和 IRQ endpoint grace；QEMU 正常退出但没有 success marker 仍视为失败。

一次 RISC-V net-loopback hang 的 GDB 计数中，物理 timer edge 超过 140 万次，而真正取得
`Firing` token 的逻辑事件只有 40 次。旧 handler 对 `Ignored` 直接返回，没有静默残留的
物理 source，因而 IRQ storm 饿死了 task deadline 和网络 worker。确定性回归必须断言
`ClockEventIrqClaim::Ignored` 产生 `ClockEventAction::Stop`；QEMU 只作为该不变量的端到端
证据，不能替代最低层红绿测试。
