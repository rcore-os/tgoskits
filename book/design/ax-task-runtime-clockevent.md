# ax-task 任务期限与 ax-runtime 物理时钟事件

## 文档状态

本文定义 ax-task 迁移后的 timer、hard IRQ、CPU-local 和物理 clockevent 所有权边界。它正式替代“`components/ax-task` 必须与 PR #1596 字节一致”的旧要求。

设计只覆盖任务调度语义，不引入通用 callback timer 服务。

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

- `clockevents_program_event()` 是物理事件编程边界；
- `hrtimer_interrupt()` 先失效已触发的 event，再处理有界 hard timer，并统一计算下一 event；
- `tick_program_event()` 连接通用 tick 与 per-CPU clockevent；
- PREEMPT_RT 只把非 hard hrtimer callback 移到 soft/threaded 上下文，显式 hard timer 仍可在硬中断执行；
- scheduler placement/migration 由 owner runqueue 保护；
- `irq_work` 与 scheduler IPI 遵守“先发布 work，后发送门铃；handler 先 claim 旧门铃”的顺序。

TGOSKits 采用相同的所有权与排序，不复制 Linux callback 形态：ax-task 发布调度期限，ax-runtime 独占物理 clockevent。

## ax-task 的所有权

每个 `CpuLocal` 独占一个固定容量 `TaskDeadlineQueue`。条目必须是 generation-bearing 的值记录，只允许：

- sleep、park、wait timeout；
- RR、Fair、Deadline/CBS/GRUB 的调度期限；
- ax-task 自身为推进 deferred task-work 所需的期限。

条目禁止保存：

- 任意闭包或函数 callback；
- OS/进程/驱动对象；
- 未受 generation 保护的裸指针；
- 需要在 IRQ 中析构的所有权。

每个 embedded timer node 最多有一个 active heap entry。rearm 物理替换旧项，cancel 物理移除旧项，不以 tombstone 消耗容量。

### ParkTicket

`ParkTicket` move-own 一个 park generation 及其可选 deadline token。取消流程先校验 owner CPU、thread generation 和 token，不提前消费 ticket。owner mismatch 或可重试失败必须保持 ticket 与 heap entry 原样。

只有两种情况可以清除 ticket：

1. 精确匹配的 heap entry 已删除；
2. expiry path 已经取走同一 generation，scheduler safe point 将按 generation 决定 timeout 是否胜出。

notify 与 timeout 只能有一个 winner。

### TaskDeadlineUpdate

本 CPU 最早任务期限改变后，ax-task 发布：

- 单调递增且非零的 generation；
- `Option<MonotonicDeadline>`；
- 是否有必须在 safe point 处理的 deferred work。

runtime 只能丢弃旧 generation，不得以相同 deadline 值推断 publication 已被处理。

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
`ClockEventFiringToken`；finish 与 panic recovery 必须消费该 token。若 CPU 已经过一次
offline/re-online，旧 token 只能失效，不能提交到新周期。

`LocalClockEvent` 是以下状态的唯一存储：task generation、task deadline、periodic deadline、deferred-work flag 和当前物理 arm。禁止旁路 scalar cache。

### 重新编程规则

在 `Armed` 状态：

- selected minimum 变早：重编程；
- selected minimum 变晚：同样重编程；
- 删除最后来源：stop device，进入 `Idle`；
- 语义状态完全相同：不写设备。

`Firing` 期间只更新逻辑 source state。handler 结束时从最新 task deadline 和 periodic deadline 计算一次 authoritative minimum，并且只提交一次硬件动作。

物理 IRQ 先执行 claim：

- `Offline/Idle/Firing` 收到的 spurious edge 直接忽略；
- edge 早于当前 `armed_deadline` 时只重编程当前 arm，不调用 ax-task；这同时处理
  offline 前残留、re-online 后才交付的 pending edge；
- 只有当前 arm 已到期时才取得 firing token 并进入有界调度处理。

### 上下线

early platform 初始化把 source 放在 masked、non-firing 状态。online 顺序为 program finite deadline，再 unmask。offline 先 mask/stop 物理源，再允许 scheduler 发布最终 `Offline`。re-online 重新执行 program-before-unmask。

CPU 数上限统一使用：

```text
min(platform_cpu_count, CPU_CAPACITY)
```

### 时间转换

- 无期限使用 `None`；
- ns 到 tick 向上取整；
- 已过期或 sub-tick deadline 钳制到设备最小非零 delta；
- 超出设备参数宽度时饱和；
- 架构 absolute-counter/alignment 运算前先完成饱和，禁止回绕为早期时间。

## hard IRQ 顺序

平台控制器和 timer device 在 runtime handler 前 claim/ACK 或失效 delivered event。runtime 顺序固定为：

1. claim 当前 arm；旧/早到边只重编程并返回，已到期边进入 `Firing(token)` 并忘记旧 arm；
2. 推进 periodic source；
3. 调用 ax-task 的 bounded `on_clock_event(now, budget)`；
4. 发布 reschedule 与 deadline/deferred-work sticky state；
5. 合并 handler 期间所有 source update；
6. 统一 program 或 stop 一次；
7. 返回平台完成 EOI。

步骤 3 到 6 都受 firing token 的 CPU epoch 约束；旧 token 的 finish/recover 不得发布
逻辑 source 或物理动作。

hard IRQ 必须：

- 无分配、无 free；
- 无睡眠和无等待外部 owner；
- 工作量受 budget 限制；
- 不执行任意 callback；
- 不持有 Starry、驱动或进程对象裸指针。

过期 task deadline 只复制到预分配 CPU-local buffer。真正的 thread wake、callback 和资源回收在 scheduler safe point 或 task worker 执行。

### batch 耗尽

预算耗尽时同时发布 sticky deadline work 和 `need_resched`。safe point 在 drain 前 claim 旧 publication；若仍有 remainder 或并发新 publication，再发布新 sticky work。旧 completion 不得清掉新工作。

### 正确性恢复

物理 timer 只是加速路径。每个 scheduler safe point 会检查 cached minimum；若期限已经过期，则提升一个 bounded batch 到 expired buffer。这样即使硬件边丢失、过晚或 CPU 因其他 `need_resched` 无法进入 idle，sleeper 仍能推进。

恢复路径同样禁止任意 callback。

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

ax-runtime 的 `SchedulerIpiDoorbell` 是唯一物理 coalescer。ax-task 不保留第二套 claimed epoch 或 IPI acknowledgement API。

## IRQ endpoint 独立生命周期

timer/IPI/UART/perf 等 IRQ-visible 数据应与上层对象拆开：

```text
Unpublished -> Published -> Draining -> Dead
```

撤销时先关 producer admission 和推进 generation，再 mask/ACK 物理源，最后等待本地 IRQ reader/owner-CPU grace。只有进入 `Dead` 后，task worker、wake target、ring 或 OS extension 才能释放。

IRQ endpoint 只保存固定值状态和稳定 registration；任务态对象通过 `Arc`、generation token 或 move-only lease 保活。不得以“外部通常不会同时销毁”为安全条件。

`IrqWaitCell` 的 registration 使用 `Detached -> Attached -> Notifying -> Draining -> Detached`。IRQ 完成 direct wake 后只进入 `Draining`，不立即开放同地址节点复用；任务侧 move-only `IrqWaitToken` 先撤销 publication，转换为 `IrqWaitDrain`，再在 notifier grace 完成后通过 `try_finish()` 开放复用。registration 最终 Drop 对应 `Dead`。正常 API 路径不泄漏，hard IRQ 不等待也不析构；只有调用者显式遗忘 token、违反回收协议时，Drop 才以泄漏代替 UAF，这是 Rust 允许的失效安全兜底，不是正常生命周期的一部分。

## 通用 timer 消费者

VM 和 POSIX callback timer 不进入 ax-task。

### AxVM

AxVM 使用 CPU-affine task worker。worker 用 task deadline 睡到 timer wheel 的下一期限；插入更早 VM timer 时，通过 bounded IRQ-safe endpoint 唤醒。VM callback 只在线程上下文执行。

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
- 无物理 IRQ 的 overdue safe-point recovery；
- park notify-vs-timeout 唯一 winner；
- owner mismatch cancellation retry；
- IPI consume/publish 生命周期；
- switch-tail 顺序与失败重试；
- CPU offline/re-online；
- IRQ endpoint revoke/quiesce/reclaim。

loom 覆盖 generation publication、publish-before-IPI、park 唯一 winner、`IrqWaitCell` notify/drain、同地址 pointer ABA 和 doorbell claim race。

UART 测试覆盖 hard IRQ 无分配/无阻塞、有界 drain、overflow、worker wake race、`try_write` 与 emergency/normal TX 互斥。

目标 crate test/clippy 后，串行运行四架构 ArceOS 与 Starry QEMU。只接受正式 success regex。hang 用 GDB 检查 timer begin/finish、IPI consume、idle commit、switch tail 和 IRQ endpoint grace；QEMU 正常退出但没有 success marker 仍视为失败。
