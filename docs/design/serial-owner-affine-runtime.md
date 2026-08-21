# 串口 owner-affine runtime 与 TTY 事务语义

## 问题与范围

当前 `dev` 已经为每个 UART 建立一个固定 CPU 的维护线程，并使用有界 RX/TX 队列在
hard IRQ、日志生产者和 TTY 之间传递数据。但寄存器能力仍只有 task-side `UartPort`
与 IRQ-side `UartIrq` 两部分，early console 的退出也只是单向布尔标记。这使三个边界
无法由类型和状态机证明：

- worker、IRQ、panic/FIQ 与 early console 是否可能同时访问同一组 MMIO 寄存器；
- UART source 被 mask 后由谁恢复，以及 rearm 窗口内的新事件是否会丢失；
- `TCSETSW`/`TCSETSF` 等待输出、配置硬件和发布软件 termios 时，普通 writer 是否会
  插入，配置失败是否会留下软件/硬件状态不一致。

本设计服务 ArceOS/StarryOS/Axvisor 的 interrupt-driven UART runtime，以及仍需在启动
和 panic 阶段输出诊断的调用方。成功标准是：每个寄存器访问都能归属于一个明确
endpoint，hard IRQ 工作有固定上限，runtime 接管失败进入确定的关闭状态，TTY 配置失败对用户态返回
稳定 errno，并由确定性测试覆盖关键并发窗口。

本次明确不引入公共 channel、全局 lock-free MPSC、自适应高低水位、串口专用调度类或
RT priority。RX SPSC、TTY 有界 `SpinLock` 队列、控制队列和 `IrqNotify` 继续承担原有
职责；普通内核日志改用 runtime 私有的每 CPU 有界 record ring，避免日志生产者争用同一
TX ingress，并使日志背压不再占用 TTY 容量。扩大调度器或通用通信抽象不能直接修复
寄存器所有权问题。

`ax-runtime` 对外只提供两类控制台语义：`emergency_console` 是同步、不可睡眠的阻塞直达
输出，`console` 是普通任务上下文的可睡眠输入输出。后者供 ArceOS、StarryOS 和 Axvisor
复用，不引入第二个 active-console 状态，也不承载 TTY ABI：原始 RX 错误、字节输出和完整
日志记录仍由既有 owner worker 传输；CR/LF、canonical、echo、termios、前台进程组和
guest 虚拟 UART 语义留在各自 OS。

## 依据与方案选择

内部基线是 `dev` commit `545bb00867ee9a99114278fa95c0ef1154048fdf`。PR #1775
head `5c9b2249a330afc8b939046871558ebf445bdb0b` 中的 control/IRQ/emergency endpoint、
固定 IRQ report 和非阻塞寄存器 gate 可作为形状参考，但其 ax-task 重构、MPSC 和优先级
调度不在本设计范围内，不能整段移植。

外部语义对照 Linux v7.1 commit
`8cd9520d35a6c38db6567e97dd93b1f11f185dc6` 的 serial core、8250、PL011 与 tty
line discipline：普通 RX 中断负责有界排空硬件 FIFO 并把输入交给后续上下文；只有
throttle、软件缓冲压力、overrun 或设备错误才 mask 对应 RX source。IRQ 与后续线程同核
不是 Linux 的通用正确性要求，但 TGOSKits 当前 raw UART 契约依赖本地 IRQ exclusion，
所以本设计把同核固定作为实现不变量，而不是缓存同步手段。只有硬件 batch 未排空、
软件 ring 无法接收、overrun 或固定预算耗尽时，TGOSKits 才把 RX source 暂时交给 owner
worker 重挂载；已经排空的 IRQ 保持 source 开启。

对比过的方案：

外部控制台仲裁参考 Linux 的
[`printk`](https://docs.kernel.org/core-api/printk-basics.html) 与
[`nbcon`](https://docs.kernel.org/driver-api/tty/console.html) producer/console-owner 分工，
以及 Zephyr 的
[`logging`](https://docs.zephyrproject.org/latest/services/logging/index.html) deferred backend 和
[`shell`](https://docs.zephyrproject.org/latest/services/shell/index.html) 日志重画集成。这里复用
其“完整记录由单一 owner 输出、交互层负责重画”的边界，不照搬其 ABI 或调度模型。

| 方案 | 结论 |
| --- | --- |
| 保持两个 endpoint 和布尔 handoff | 无法表达 panic/FIQ 与 early/runtime 的互斥和失败回滚。 |
| 所有寄存器访问共用 blocking/spin lock | hard IRQ 或 panic 可能等待被中断上下文持有的锁。 |
| 每次 worker 运行都关闭整个 controller IRQ line | shared IRQ 会影响其他设备，且把 source ownership 放错层。 |
| 新建公共 lock-free channel 与 RT 调度 | 增加无关并发模型；日志 record ring 只属于 runtime。 |
| endpoint 拆分、非阻塞 gate、source-level mask/rearm | 以最小公共边界表达所有权，并复用现有 runtime。采用。 |

## 所有权模型

每个 runtime UART 拆成三个 endpoint：

```text
control endpoint    owner_cpu 上的唯一维护线程
IRQ endpoint        固定到 owner_cpu 的注册回调
emergency endpoint  panic/FIQ 的同步输出路径
```

三者引用同一个 `UartRegisterGate`。worker 在保存并关闭本地 IRQ 后进入 gate；IRQ、FIQ 和
panic 首次接管都不能偷取正在执行的普通事务。普通 guard 退出后恢复空闲；panic 使用有界
重试取得 emergency ownership，成功后直到关机都不归还，后续格式化调用直接复用该
ownership，普通 worker 与 IRQ 因而不能插入 fatal record。gate access 不可跨线程移动。
endpoint 只负责 raw UART 寄存器语义；IRQ 注册、CPU affinity、worker 唤醒和 TTY 适配仍
属于 OS runtime。

early console 是第四个、仅在 runtime 接管前有效的所有者。接管状态为：

```text
Early -> Preparing -> Runtime
                   -> FailedClosed
```

`Preparing` 首先阻止新的 early RX、TX 和 IRQ 寄存器访问，再等待已经进入的 early 访问
退出。probe 只建立 dormant runtime、注册保持 disabled 的 controller IRQ，不写任何 UART
寄存器。精确匹配的 firmware console 已经完成线路、FIFO 和波特率配置，handoff 因而直接
adopt 该状态：只 mask device-local source、启用已注册的 IRQ action 并发布 runtime 输出路由，
不调用普通串口的 `startup()`，也不清 `UARTEN` 或等待正在按线速排空的 TX FIFO。只有这些
步骤全部成功后才提交 `Runtime`；其他非 console UART 仍在显式 open 时执行 startup/config。
一旦为已探测的 runtime UART 进入 `Preparing`，提交前任何失败都进入 `FailedClosed`；公共层
不会把一次 runtime 启动失败重新解释为“未探测到 driver”并回退到 raw HAL。该状态吞掉普通
输出并禁止输入，避免双 owner 再次触碰未知硬件状态。`FailedClosed` 同时是被选中 UART
runtime 自身的终态：即使它仍出现在已发现设备表中，per-port start、RX lease 与 TX 也不得
把它当成普通 `ttyS*` 重新打开。进入该状态会先发布 runtime 终态并关闭 IRQ，再同步把
register gate 转为 terminal ownership、mask device-local source；fail-close 返回后 normal
worker 和 IRQ endpoint 都无法重新取得寄存器，不依赖 watchdog 或延时清理任务。

当 runtime 同时具备 `irq + multitask` 时，公共层自动在 scheduler、IRQ、设备探测和 serial
worker 就绪后、但在第一个 AP 启动前尝试接管，不再要求 OS 配置 `serial` 或
`runtime-console` feature。firmware 给出硬件 `DeviceId` 时只接受精确匹配；只有
`NotSpecified` 才按统一 `ttyS` 编号回退到 `ttyS0`。若没有探测到匹配的 runtime UART，
保持原始 HAL 为唯一 owner；已经进入 `Preparing` 后 adopt、IRQ 启用或提交失败则进入
`FailedClosed`。成功提交后禁止 SMP 阶段重新落回 early UART。

panic/FIQ 不反向调用 platform/runtime callback，也不借用 control endpoint。首次接管使用
固定次数的非睡眠 gate 重试，避免 panic 打断同核 owner 时永久自锁；接管失败就丢弃本次
记录并更新统计。gate 只有在已经永久排除普通 register transaction、并在设备寄存器层 mask
该 UART 的 IER/IMSC 后，才返回可写 emergency capability；调用者不存在“先写、以后再 mask”
的状态。该过程绝不关闭可能共享的 controller line。后续每次 raw pass 只写固定预算字节，
不恢复普通 owner 的 interrupt mask；runtime 重复这些有界 pass，直接流式写完完整格式化记录，
不再把 fatal record 截断在固定软件缓冲区。

这个接口是同步阻塞直达而不是排队等待：`emergency_console::write_fmt` 不分配、不睡眠、
不等待 worker。成功接管后它轮询硬件直到记录写完并返回源字节数；若首次 gate 接管失败
则返回 0。该语义优先保留完整 panic/backtrace 诊断，硬件永久不 ready 时可能停在 fatal
输出，属于同步 emergency console 的明确边界。

## IRQ 与 worker 协议

hard IRQ 只允许：

1. 读取和确认该 UART 的 source/status；
2. 向固定 64 项 `SerialIrqReport` 放入 RX sample；
3. mask 需要 deferred service 的 UART source；
4. 发布 report 并用 `IrqNotify` 唤醒 worker。

IRQ 中禁止分配、阻塞、调用 runtime/platform callback、操作 TTY 或关闭 interrupt
controller line。共享 IRQ 返回 `None` 表示本 UART 未产生事件。

TX source 命中后立即 mask，并在 report 中加入 `TX_SPACE` rearm。worker 消费现有有界
TX ingress，直到当前预算或 FIFO 空间用尽；只有仍有待发送字节时才执行 rearm，软件输出
为空后立即清除 TX pending，避免把“移位寄存器尚未清空”错误表示成可由 TX IRQ 推进的工作。

RX IRQ 在驱动固定预算内排空当前硬件 batch。只有驱动报告仍有未排空样本、固定 report 已满、
硬件 pass budget 用尽、runtime IRQ RX ring 无法接受整个 report 或发生 overrun 时，才 mask
对应 RX source 并交给 owner worker rearm；完全排空的 IRQ 保持 source 开启，避免小 FIFO 在
worker 获得调度前溢出。设备 fault 仍会 mask 全部 source 并停止 runtime。

worker 先排空 report/RX ring，再执行重挂载。如果 rearm 立即报告硬件仍 readable，且此时
IRQ ring 中没有 sample，worker 必须选择 `Port` 路径直接排空硬件 FIFO，不能把这个事件误当
成一个空的 `Irq` 路径。下游 TTY subscription 满时保留一个 `pending_rx`，保持 RX masked；
consumer 释放空间后沿现有 notify 路径唤醒 worker。重新开启 source 必须使用固定协议：

1. 写 UART IER/IMSC enable；
2. 立即重新检查 readiness；
3. 已 ready 则再次 mask，并把 immediate event 返回 worker。

该协议同时关闭清中断、enable 与新字符/新 TX 空间之间的丢事件窗口。任何 mitigation 只操作 UART
自己的 IER/IMSC，禁止调用 `disable_irq()` 屏蔽 shared controller line；controller handle
只用于 runtime startup/shutdown 和注册失败回滚。

RX 或 TX 的单次预算耗尽时，worker 保留已有 pending/rearm/notify 状态，并在释放 UART
register gate 和相关锁后主动让出一次调度机会，再继续处理本轮另一方向或进入下一轮。
这样持续串口积压仍会推进，RX 也不会跳过同轮 TX，但固定的 `owner_cpu` 不会成为无界运行
段，source mask/rearm 的所有权保持不变。

## SMP 与内存顺序

IRQ affinity 和 worker cpumask 都固定到同一 `owner_cpu`，以满足当前 `UartPort` 的本地
IRQ exclusion 契约，并避免 hard IRQ 跨核等待寄存器 gate。其他 CPU 只能通过现有有界
TX ingress、控制队列和 RX subscription 访问 runtime，不能访问 UART 寄存器。

这些 CPU 间队列运行在 x86_64、AArch64、RISC-V 和 LoongArch 的 coherent SMP 内存上；
发布 head/state 使用 Release，观察 tail/state 使用 Acquire，合并门铃使用 AcqRel。不需要
对普通 CPU 内存做显式 cache flush。平台必须在启动 secondary CPU 前明确承诺普通
cacheable RAM 对所有 online CPU coherent；无法建立该契约的平台不得进入通用 SMP
runtime。仅给串口队列增加 clean/invalidate 不能修复 scheduler、锁、引用计数和 IPI
共享状态，因此本设计不提供所谓 non-coherent mailbox fallback。

`dma-api` 只表达 CPU 与设备之间的 DMA ownership、方向和 cache maintenance。CPU 间
mailbox 不伪装成 DMA mapping，也不使用 `DmaDirection`。将来若 UART 使用 DMA，buffer
ownership 与 cache 方向必须另由 `dma-api` 表达，不能套用普通 SMP record ring 的结论。

## RT 日志 mailbox

本设计借鉴 PREEMPT_RT 的 producer/console-worker 分工，而不引入 RT 调度优先级：普通
`ax_print!` 和 `log` 调用只完成固定容量 record 的有界发布，UART owner worker 是 runtime
阶段唯一执行 TX FIFO MMIO 的上下文。TTY 的显式 write/drain 仍可睡眠，但使用独立 TX
ingress；日志 backlog、覆盖或 reservation failure 不消耗 TTY 队列空间。

每个配置 CPU 拥有一个 64-slot record ring，每个 record 最多携带 1024 字节。task 与 IRQ
虽然可能在同一 CPU 上交替成为 producer，但发布期间禁止迁移和本地 IRQ，所以从 ring
角度仍是单 writer；FIQ/NMI 和 panic 不进入该路径。owner worker 是所有 ring 的唯一
reader，并在 CPU 之间 round-robin：保证每个 CPU 内的 sequence 顺序，不承诺不同 CPU
调用之间的全局时间顺序。IPI/`IrqNotify` 只是可合并 doorbell，不能承载 payload 或定义
record 顺序。

每个 slot 把 generation 与以下状态一起原子发布：

```text
FREE -> WRITING -> READY -> READING -> FREE
          ^          |
          +----------+  producer 仅可回收尚未被 reader 取得的 READY
```

producer 可以像 Linux printk ring 一样复用最旧的 `READY` record；若目标 slot 已是
`READING` 或 `WRITING`，本次 reservation 立即失败。reader 必须先 CAS 取得 `READING`
ownership，复制完成后才释放，禁止 producer 与 reader 同时访问 record payload。每 CPU
单调 sequence 既用于 generation，也让 reader 统计覆盖或 reservation failure 形成的 gap。
超长 UTF-8 消息在 record 边界内截断并标记；递归 publish、runtime 未就绪或 slot busy
均只更新有界统计并返回，不能等待 worker、分配内存或进入 TTY。

secondary CPU 在建立本核 scheduler/current task 前也可能发布启动日志。该阶段只允许把
完整 record 发布到本 CPU ring，不能通过 `IrqNotify` 选择运行队列或唤醒固定在 owner CPU
的 worker；否则会在本核 scheduler/IPI 尚未初始化时进入跨核 wake。每 CPU 的显式
`wake_ready` 状态只在 scheduler、IRQ 和 IPI 路径全部就绪后发布；之后的普通日志或 IRQ
日志才发送可合并 doorbell，并同时推动此前缓存的早期 record。

RX hard IRQ 在驱动给定的固定 budget 内直接抽取样本。只有驱动明确报告 batch 未排空、
overrun 或 IRQ pass budget 耗尽，或者 runtime 的预分配 ring 已满时，公共层才 mask RX 并
交给 owner worker rearm。已经排空的 IRQ 保持硬件 RX source 开启；不能把每个小 batch 都
无条件 deferred 到任务态，否则 16550 等小 FIFO 会在 worker 获得调度前按线速溢出。

early 阶段仍可通过 early endpoint 做有界直接输出；`Preparing` 阻止新访问，`Runtime`
提交后普通日志只能进 mailbox，`FailedClosed` 丢弃普通日志。panic/FIQ 只尝试 emergency
endpoint，不能排空 record ring 或等待 owner worker；一旦成功接管，排队中的普通记录
直接留在 runtime 队列中，不得再访问 UART。

## TTY 事务与锁顺序

serial backend 使用同一个 sleepable output lock 串行化 write、echo、drain、discard 和
termios hardware update。termios 修改固定顺序为：

```text
termios-update -> output -> terminal-termios
```

获取 line-discipline lock 前必须释放 output lock，避免与既有
`ldisc -> echo/output` 路径形成反向嵌套。

- `TCSETS`：在 output lock 内配置硬件，成功后发布新 termios；
- `TCSETSW`：在同一次 output lock 持有期间 drain、配置、发布；
- `TCSETSF`：完成 `TCSETSW` 事务并释放 output lock，然后获取 line discipline 并清输入。

硬件配置失败时不发布新 termios。无效参数返回 `EINVAL`，驱动配置阶段的有界硬件等待
超时返回 `ETIMEDOUT`，其他寄存器/设备错误返回 `EIO`。用户内存复制必须在上述锁外完成。

owner worker 在普通状态下有界交替服务 TTY frame 与日志 record。drain 或 termios barrier
进入后提交一个由 worker 唯一持有的 `DrainTx` 控制事务。worker 先完成已经取得的 pending
record，随后暂停取得新日志；只有 TTY ingress 为空、pending TTY frame 为空且 UART
FIFO/shift register 报告 idle 后，才完成事务。普通状态不查询或保存 hardware-idle 状态；
若 UART 没有 shift-register completion IRQ，drain 事务让出调度器后重新检查，而不创建
定时任务或依赖超时兜底。因此持续日志流不能饿死 `TCSETSW`/`TCSETSF`，barrier 也不会
丢弃已经接受的 TTY 输出。配置发布或失败回滚完成后，worker 恢复日志 round-robin。

## 公共任务态控制台与日志仲裁

公共任务态接口把活动后端拆成三个 capability；它们都属于同一个 `console` 接口族，日志
subscription 只是 runtime 输出仲裁的附属 lease，不是第三条物理输出路径：

- 唯一 `TaskConsoleInput`：非阻塞读取原始 `RxItem`、`WaitQueue` 睡眠等待、阻塞读取、
  discard 和 poll source；IRQ/worker 发布 RX 后同时唤醒普通输入和组合 console event。
- 可克隆 `TaskConsoleOutput`：所有克隆共享 runtime 原有 sleepable output lock，提供
  非阻塞 `try_write`、可睡眠 raw/text write、真实 UART drain、discard、poll 和串行化
  reconfigure 事务。一个调用的所有 frame 在锁内提交，多 writer 不能交叉。
- 唯一可选 `ConsoleLogSubscription`：worker 只在取得一条完整 mailbox record 后检查
  订阅状态；短 gate 临界区保证订阅/释放不会切开 record。订阅队列固定 64 条，worker
  永不等待；满时按完整 record 丢弃并累计 record/源字节统计。释放订阅后恢复直接 UART
  输出。panic/emergency 不进入该队列，继续尝试 emergency endpoint。

没有探测到匹配 UART driver 时，同一 `TaskConsoleInput`/`TaskConsoleOutput` 类型内部选择
raw HAL 后端，而不是让 ArceOS、StarryOS 或 Axvisor 再维护 fallback 状态。若 HAL 提供
console IRQ，公共层注册唯一 handler，在 hard IRQ 中把 FIFO 排空到固定容量 SPSC queue，
再通过 IRQ-safe `WaitQueue` 和 poll source 唤醒唯一 `TaskConsoleInput` reader；软件 queue
满时保留 overrun 状态。任务态绝不直接轮询硬件，也不靠 `yield_now` 或 CPU affinity
维持进展。HAL 没有 console IRQ 时，raw output 仍然可用，但 `take_input` 明确返回
`OperationNotSupported`，调用方可以关闭交互入口而不制造伪睡眠 capability。raw output
的所有克隆共享公共 sleepable task lock；实际访问 raw hardware 前再取得 IRQ-safe
hardware lock。普通完整日志只非阻塞尝试 hardware lock，不能触碰依赖 current task 的
sleepable lock，因为日志既可能来自 hard IRQ，也可能来自尚未执行
`init_scheduler_secondary` 的 AP。固定锁序为 task lock → hardware lock，日志仅取后者，
因此完整 record 不与任务输出交叉，也不会让 pre-scheduler AP 因查询 mutex owner 进入错误
状态。raw HAL 没有 owner worker，因此不提供日志 subscription、硬件 reconfigure 或比
平台同步写更强的 drain 保证。已经进入 `Preparing` 后失败的 `FailedClosed` 后端不会走
这条 fallback，也不会重新访问 early UART。

console 不再有独立 Cargo feature。`ax-runtime` 在同时启用既有 `irq` 和 `multitask`
能力时编译多 UART runtime、紧急端点与任务态 console；probe 后有匹配设备才接管，否则
公共任务态 output 自动使用 HAL，input 仅在 HAL 能提供 IRQ-backed sleep 时可用。无调度器
或无 IRQ 的最小 ArceOS 构建不会创建 owner worker，也保持原始 HAL 路径。

ArceOS 的 `ax-api`、`ax-posix-api` 和 `ax-std` 共享唯一 input lease；空输入通过
`wait_readable` 睡眠，不再 `yield_now` 轮询，stdout `flush` 等待真实 UART idle。
Starry 的 console `ttyS*` 取得公共 input，全部 runtime UART 使用 serial 层的 per-port task
output capability，从而共享各 runtime 自己的 output lock；Starry 不再自行选择、handoff 或
维护第二把 output lock。其他串口仍按 open 生命周期启动，line discipline 和 Linux TTY ABI
不变；per-port UART 能力不反向扩张公共 console 的 active-owner API。

Axvisor 在打印 banner、发布普通启动日志或启动任何 vCPU 前取得 input、output，并在 runtime 后端可用时取得 log
subscription。`TaskConsoleOutput` 随后 move 给唯一的 `axvisor-console-output` 任务；
GuestConsoleMux、虚拟串口 backend 和其他可能位于 vCPU 禁止抢占区内的 producer 只在
backend 创建阶段预分配每 guest 的 16 KiB backlog；回调使用无分配的流式格式化，在
`NoPreemptMutex` 下把同一 transaction 直接写入固定 64 KiB 字节队列，不取得 sleepable
output lock，也不等待 UART 背压。该任务按 512 字节批次调用公共可睡眠 output；队列满时
完整回滚并丢弃当前 transaction，在下一批前报告丢失总数，已经排队的 transaction 不会被
截断；物理 output 失败时停止接受新提交并通过 emergency console 报告终态，不创建 timeout
任务、重试 owner 或回退到平台轮询输出。
每 guest backlog 满后保留最新字节并累计被淘汰的源字节；下次形成完整 boot 行或切为该 guest
前台时，先用无分配 decimal formatter 输出丢失摘要，再回放保留内容。该热路径只在注册阶段
为 `VecDeque` 预分配固定上限，满队列执行 pop/push，不触发扩容。

HAL 不支持 IRQ-backed input 时，Axvisor 的 console task 只消费宿主日志或永久睡眠，不创建
轮询输入任务；管理面和 VM 启动不受影响。管理 shell 的 prompt、命令输出和行编辑都进入同一
host-output 队列，并把
“清当前行、输出完整日志、重画 prompt/内容/光标”合成一次物理输出事务；boot multiplex
把宿主日志作为独立完整行。guest 进入 interactive foreground 后，宿主日志不得进入
guest RX 字节流，而按完整 record 缓存在 16 KiB host backlog；detach 时先报告丢失条数
和源字节，再按 record 回放。超过容量只删除最旧完整 record，不制造残缺日志行。
guest RX ingress 同样有界；队列首次满时发布一条完整宿主日志，后续丢弃不重复刷屏，guest
实际读出数据腾出空间后才允许报告下一次 overflow，因此输入丢失不会静默发生，也不会从
宿主 warning 反向注入 guest UART 字节流。

## 验证与回滚

最低层确定性测试覆盖 gate contention、worker/IRQ/emergency MMIO 互斥、固定 report
容量、TX mask/rearm、RX deferred 条件与 rearm、RX budget/queue-full/overrun 状态、rearm
窗口 immediate event 选择 `Port` 路径、shared IRQ 不关闭 controller line、PL011 配置 timeout/寄存器回滚，
emergency 永久接管与超过 1 KiB 的 fatal record 流式输出，以及 termios 并发 writer 和错误传播。日志 mailbox 另外覆盖 slot generation/state、回收
`READY`、拒绝覆盖 `READING/WRITING`、sequence gap、每 CPU FIFO、跨 CPU round-robin、
递归发布、UTF-8 截断、日志 flood 与 TTY 容量隔离。

mailbox 状态机的 host 确定性测试位于
`os/arceos/modules/axruntime/src/serial/log_mailbox.rs`，日志与 TTY 容量隔离的旧实现失败
回归位于同目录的 `ingress.rs`。真实 SMP 探针由 Starry `axtest_kernel` 调用，只在
`cfg(axtest)` 下编译：四个固定 CPU 的 producer 同时发布包含 CPU、本核 sequence 和
checksum 的 record，测试 reader 验证 round-robin、核内 FIFO、完整性、零 gap 与零丢失，
并确认日志 ring 已占用时独立 TTY ingress 仍可接受完整容量。`axtest_kernel` 的 Cargo
test target 通过 `required-features` 启用 `smp`，四架构都由 package 目录中的
`os/StarryOS/kernel/qemu-<arch>.toml` 默认配置明确使用 SMP=4，例如：

```bash
cargo xtask ktest qemu -p starry-kernel --test axtest_kernel \
  --arch aarch64
```

Starry grouped 回归新增 `test-tty-termios-transaction`，并保留
`tty-bugfix-bug-raw-terminal-polling`、`test-tty-flush` 与
`tty-console-input-burst` 的原有断言。合入门槛是在 x86_64、riscv64、aarch64、
loongarch64 上串行运行 SMP=4 QEMU 用例；当前是否满足以对应 PR head 的终态 CI 为准，
设计文档不把历史运行结果当成新 head 的验证证据。

Axvisor 的 `qemu-console-interleave/interleave` 是 #2108 的确定性红灯：先经公共任务态 output
直接输出 `rm`，
再发布以 `:` 开头的宿主完整日志，最后结束交互片段。旧平台轮询/early UART 路径稳定形成
`^rm:`；修复后 shell 必须实际消费该订阅 record，测试以独占一行的宿主记录作为成功见证，
并仍保留原 `(?m)^rm:` fail regex。host mux 单元测试另外覆盖 open guest line 分隔、管理 shell
显示、foreground 完整记录缓存、16 KiB oldest-record 丢弃和 detach 回放摘要。

`qemu-console-atomic-output/atomic-output` 另行启用抢占调度，在 `PreemptGuard` 内用
`try_write` 填满公共 runtime TX ingress，直到明确返回 `WouldBlock`，最后经真实的固定
host-output 队列提交成功标记。它直接验证 atomic producer 到唯一 output task 的 transport
边界；`GuestSerialBackend` 的 generation、格式化与事务提交另由 mux/axtest 覆盖，不能把该
单 CPU QEMU case 表述为真实 VM lifecycle 证明。旧的同步 guest output 调用会在同类背压下
进入 `TaskConsoleOutput::write_all` 并命中 atomic-context panic；新边界只提交固定队列，guard
释放后由唯一 output 任务完成标记，不使用延时释放、watchdog 或 timeout 修复错误状态。

这个迁移不能只替换公共接口而保留 OS 私有 owner、轮询 reader 或第二把 output lock。
已经开始 handoff 后的单次接管失败必须 fail closed，不允许静默退回 raw polling 或
controller-line mitigation；只有“未探测到匹配 driver”才选择公共 raw HAL 后端，并且
该后端的任务输入仍必须由 IRQ 和有界 queue 驱动。
