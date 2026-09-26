# resume888: Linux RT 普通 sleeper 的软定时器执行上下文

本次为只读语义核对。Starry 源码为
`b292a098bb60ef604e7677c37cd95d926ff08200`，本机 Linux 源码为
`/home/zhourui/linux-src@8cd9520d35a6c38db6567e97dd93b1f11f185dc6`，
`make kernelversion` 输出 `7.1.0`。Linux 本次核对的文件无本地改动。
没有修改 Starry 源码、构建镜像或运行板卡/full20。

`kernel/time/hrtimer.c:2220-2248` 的 `__hrtimer_setup_sleeper()` 在
`CONFIG_PREEMPT_RT` 下仅当 `rt_or_dl_task_policy(current)` 且未显式请求
soft 模式时，把 sleeper 设为 `HRTIMER_MODE_HARD`；注释明确提到普通用户
线程可在同一到期时刻大量唤醒，故不把全部 sleeper 放在 hard IRQ。
`hrtimer_nanosleep()` 通过 `hrtimer_setup_sleeper_on_stack()` 使用这个选择。
`kernel/softirq.c:1121-1162` 为强制线程化的 timer softirq 注册
`ktimers/%u`，以低 FIFO 优先级执行。`include/linux/interrupt.h:510-519`
还规定 `CONFIG_IRQ_FORCED_THREADING` 与 `CONFIG_PREEMPT_RT` 同时启用时
`force_irqthreads()` 恒为真；`kernel/softirq.c:1165-1173` 因此注册该
per-CPU 线程。本结论以 issue 冻结的 PREEMPT_RT 配置为前提。

Starry `components/ax-task/src/thread/current/park.rs:459-477` 同样让
FIFO/RR/Deadline 走 ParkHard，OTHER/Fair 走 ParkSoft；
`components/ax-task/src/runtime/service/ktimer.rs` 以专用 FIFO1 worker
领取、完成 soft timer 并唤醒 sleeper。这一执行者边界不是已证明冗余的
实现细节，而是与 Linux 7.1 RT 对照的调度策略相符。

`resume887` 提出的在 IRQ 后调度入口直接执行 ParkSoft 到期，会绕过
普通 sleeper 的 `ktimers` 线程执行边界，不能作为与冻结 Linux RT 等价的
90% 优化。Starry `runtime/switch/dispatch.rs:77-117` 的调度入口还持有
`RuntimeSchedulerFrameGuard`；`runtime/context.rs:268-299` 明确该 guard
持有 IRQ-off scheduler baton。当前 `ktimer` service 包含任意 kernel
soft callbacks，并在 worker 中于 deadline-base 锁外执行，不能直接搬入
这个调度帧。它需要全新并发与优先级语义设计，不是局部快路径。

结论：拒绝 `resume887` 的执行者替换假设；保留 ParkSoft/ParkHard 分流。
旧 `resume831` 已把 notify-return 到 scheduler-entry、worker-decision、
claim 分段，重测同一段不提供新的等价优化。继续寻找不改变 ktimers
执行上下文且可证明消除实际成本的路径；本次没有原生性能收益。
