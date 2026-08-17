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
endpoint，hard IRQ 工作有固定上限，runtime 接管可回滚，TTY 配置失败对用户态返回
稳定 errno，并由确定性测试覆盖关键并发窗口。

本次明确不引入公共 channel、lock-free MPSC、自适应高低水位、串口专用调度类或 RT
priority。现有 RX SPSC、TX 有界 `SpinLock` 队列、控制队列和 `IrqNotify` 足以承载所需
语义；扩大调度器或通用通信抽象不能直接修复寄存器所有权问题。

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
所以本设计把同核固定作为实现不变量，而不是缓存同步手段。

对比过的方案：

| 方案 | 结论 |
| --- | --- |
| 保持两个 endpoint 和布尔 handoff | 无法表达 panic/FIQ 与 early/runtime 的互斥和失败回滚。 |
| 所有寄存器访问共用 blocking/spin lock | hard IRQ 或 panic 可能等待被中断上下文持有的锁。 |
| 每次 worker 运行都关闭整个 controller IRQ line | shared IRQ 会影响其他设备，且把 source ownership 放错层。 |
| 新建公共 lock-free channel 与 RT 调度 | 增加无关并发模型，当前有界队列已经满足数据传递。 |
| endpoint 拆分、非阻塞 gate、source-level mask/rearm | 以最小公共边界表达所有权，并复用现有 runtime。采用。 |

## 所有权模型

每个 runtime UART 拆成三个 endpoint：

```text
control endpoint    owner_cpu 上的唯一维护线程
IRQ endpoint        固定到 owner_cpu 的注册回调
emergency endpoint  panic/FIQ 的有界、非阻塞输出路径
```

三者引用同一个 `UartRegisterGate`。worker 在保存并关闭本地 IRQ 后进入 gate；IRQ、FIQ 和
panic 只能 `try_enter`，失败时设置既有 pending/统计状态并立即返回，不能自旋等待。
gate guard 不可跨线程移动。endpoint 只负责 raw UART 寄存器语义；IRQ 注册、CPU affinity、
worker 唤醒和 TTY 适配仍属于 OS runtime。

early console 是第四个、仅在 runtime 接管前有效的所有者。接管状态为：

```text
Early -> Preparing -> Runtime
                   -> Early         可证明硬件状态已恢复
                   -> FailedClosed  硬件状态未知或恢复失败
```

`Preparing` 首先阻止新的 early RX、TX 和 IRQ 寄存器访问，再等待已经进入的 early 访问
退出。只有 UART startup/config、IRQ 注册和 runtime 输出路由全部成功后才提交 `Runtime`。
提交前失败必须撤销已完成步骤；能证明寄存器已恢复时回到 `Early`，否则进入
`FailedClosed`。后者吞掉输出并禁止输入，避免双 owner 再次触碰未知硬件状态。

panic/FIQ 不反向调用 platform/runtime callback，也不借用 control endpoint。它只在 gate
可用时保存必要 interrupt-mask 寄存器、mask UART source、写固定预算字节并恢复 mask；
无法进入或预算耗尽时丢弃剩余字节并更新统计。

## IRQ 与 worker 协议

hard IRQ 只允许：

1. 读取和确认该 UART 的 source/status；
2. 向固定 64 项 `SerialIrqReport` 放入 RX sample；
3. mask 需要 deferred service 的 UART source；
4. 发布 report 并用 `IrqNotify` 唤醒 worker。

IRQ 中禁止分配、阻塞、调用 runtime/platform callback、操作 TTY 或关闭 interrupt
controller line。共享 IRQ 返回 `None` 表示本 UART 未产生事件。

TX source 命中后立即 mask，并在 report 中加入 `TX_SPACE` rearm。worker 消费现有有界
TX ingress，直到当前预算或 FIFO 空间用尽；仍有数据时执行 rearm，已无数据且硬件 idle
时清除 TX pending，避免空闲 TX interrupt storm。

RX 在正常情况下由 IRQ 有界排空后保持 enabled。只有以下情况 mask
RX/timeout/error source，并把 RX 加入 rearm：

- 固定 64 项 report 已满；
- 一次 IRQ 的硬件 pass budget 已用尽；
- runtime IRQ RX ring 无法接受整个 report；
- 观察到 overrun；
- 设备 fault（此时 mask 全部 source 并停止 runtime）。

worker 先排空 report/RX ring，再在需要时直接轮询硬件 FIFO。下游 TTY subscription 满时
保留一个 `pending_rx`，保持 RX masked；consumer 释放空间后沿现有 notify 路径唤醒
worker。重新开启 source 必须使用固定协议：

1. 写 UART IER/IMSC enable；
2. 立即重新检查 readiness；
3. 已 ready 则再次 mask，并把 immediate event 返回 worker。

该协议关闭 enable 与新字符/新 TX 空间之间的丢事件窗口。任何 mitigation 只操作 UART
自己的 IER/IMSC，禁止调用 `disable_irq()` 屏蔽 shared controller line；controller handle
只用于 runtime startup/shutdown 和注册失败回滚。

## SMP 与内存顺序

IRQ affinity 和 worker cpumask 都固定到同一 `owner_cpu`，以满足当前 `UartPort` 的本地
IRQ exclusion 契约，并避免 hard IRQ 跨核等待寄存器 gate。其他 CPU 只能通过现有有界
TX ingress、控制队列和 RX subscription 访问 runtime，不能访问 UART 寄存器。

这些 CPU 间队列运行在 x86_64、AArch64、RISC-V 和 LoongArch 的 coherent SMP 内存上；
发布 head/state 使用 Release，观察 tail/state 使用 Acquire，合并门铃使用 AcqRel。不需要
对普通 CPU 内存做显式 cache flush。将来若 UART 使用 DMA，buffer ownership 与 cache
方向必须改由 `dma-api` 表达，不能套用本段结论。

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

硬件配置失败时不发布新 termios。无效参数返回 `EINVAL`，有界硬件等待超时返回
`ETIMEDOUT`，其他寄存器/设备错误返回 `EIO`。用户内存复制必须在上述锁外完成。

## 验证与回滚

最低层确定性测试覆盖 gate contention、worker/IRQ/emergency MMIO 互斥、固定 report
容量、TX mask/rearm、正常 RX 不 mask、RX budget/queue-full/overrun mask/rearm、rearm
窗口 immediate event、shared IRQ 不关闭 controller line、PL011 配置 timeout/寄存器回滚，
以及 termios 并发 writer 和错误传播。

Starry grouped 回归新增 `test-tty-termios-transaction`，并保留
`tty-bugfix-bug-raw-terminal-polling`、`test-tty-flush` 与
`tty-console-input-burst` 的原有断言。最终在 x86_64、riscv64、aarch64、loongarch64 上
串行运行 SMP=4 QEMU 用例。每个 bug 回归先在旧实现工作区证明失败，再与修复一起进入
保持绿色的 commit。

回滚整个改动时，旧 runtime 仍可恢复两个 endpoint 和单向 handoff；不能只回滚接口而
保留新调用方。运行期单次接管失败则按上述 typed state 回滚，不允许静默启用 polling
或 controller-line mitigation。
