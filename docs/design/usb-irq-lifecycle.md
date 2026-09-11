# 面向任务调度器的 USB IRQ 生命周期

## 范围与结论

本文档定义 `crab-usb` 与 StarryOS 内核态 USB IRQ 的统一生命周期。该设计在
不引入轮询、USB 私有调度器或旧任务运行时兼容层的前提下修复
[#1852](https://github.com/rcore-os/tgoskits/issues/1852)。

AArch64 上观察到的故障并非任务唤醒丢失。xHCI 在 `Core::init()` 发出 root hub
命令期间已经打开硬件中断源，但 Starry 的共享 IRQ action 仍处于 disabled。
因此，电平触发的 PCI INTx hwirq 37 持续投递，却无法进入负责完成 command
future 的 USB ACK 与事件 drain 路径。

新的所有权规则为：

> Starry 先使 framework action 与固定 event worker 就绪；随后由 `Core` 在中断源
> 保持 masked 的前提下 prepare controller，再 arm controller，最后才发出 root
> hub 命令。

此契约适用于全部 kmod backend。libusb/umod backend 不拥有硬件 IRQ source，
不属于这一生命周期。

## 状态机

```text
Discovered（已发现）
  -> ActionRegisteredDisabled（action 已注册且关闭）
  -> EventWorkerReady（event worker 已就绪）
  -> ActionEnabledAndGateActive（action 已启用且 gate 已激活）
  -> ControllerPreparedMasked（controller 已准备且中断源 masked）
  -> ControllerArmed（controller 已 arm）
  -> RootHubReady（root hub 已就绪）
  -> InitialProbe（首次探测）
  -> Ready（可用）
```

各状态转换由不同层负责：

| 状态转换 | 所有者 | 不变量 |
| --- | --- | --- |
| 注册 action | Starry IRQ registry | 共享 action 使用 `AutoEnable::No` |
| 启动 worker | Starry USBFS | 任一 action 启用前，固定 worker 已存在 |
| 启用 action | Starry IRQ registry | 同一 host 的全部 action 事务性启用 |
| prepare controller | kmod `CoreOp` backend | ring、DMA 与 MMIO 均已就绪；IRQ source 保持 masked |
| arm controller | `Core` | 在第一条 root hub 命令前完成 |
| 初始化 root hub | `Core` | 失败时 mask controller，且 `USBHost::initialized` 保持 false |
| 首次 probe | Starry `usbfs-init` worker | command/transfer future 通过标准任务 `Waker` 休眠 |

`CoreOp::prepare_controller` 不再提供可选的 IRQ 实现。每个 kmod controller 都必须
实现 prepare、enable 与 disable。xHCI 和 DWC2 不再在各自的 controller prepare
内部提前打开中断源；EHCI 原本就在 `USBINTR` masked 的状态下完成 prepare；DWC3
包装层继承 xHCI 契约。

## xHCI 编程顺序与中断所有权

实现对照 Linux v7.1 commit
`8cd9520d35a6c38db6567e97dd93b1f11f185dc6`：

- `drivers/usb/host/xhci-mem.c` 依次配置 `ERSTSZ`、`ERSTBA`、`ERDP`；
- `drivers/usb/host/xhci.c` 先打开 `USBCMD.EIE`，再打开 primary interrupter 的
  `IMAN.IE`，并回读 `IMAN` 以刷新 posted write；
- `drivers/usb/host/xhci-ring.c` ACK `USBSTS.EINT` 与 `IMAN.IP`，drain event，
  更新 `ERDP`，然后 rearm interrupter；
- command 与 transfer 结果均在 completion wake 之前发布。

prepare 阶段始终保持 `USBCMD.EIE` 和 `IMAN.IE` 为关闭状态。初始化 `IMAN` 与
`ERDP` 时，对 RW1C 的 `IP` 和 `EHB` 位写零，避免初始化过程消费刚到达的 pending
event。enable 时先打开全局 EIE，再打开 primary interrupter，并回读 `IMAN`；
disable 时先关闭并回读 `IMAN`，再关闭全局 EIE。

`ControllerIrqState` 串行化 controller enable/disable 与任务上下文的 rearm。
`XhciIrqMaskState` 由 core 和 event handler 共享，通过 Acquire/Release 内存序发布
`Unmasked`、`Masking`、`Masked` 与 `Rearming`。硬 IRQ ACK 不等待任务侧所有权。
若 ACK 在 rearm 过程中获胜，任务端会执行最终硬件 mask，而不会发布过期的
unmasked 状态。

## 唤醒与执行边界

唤醒链条按职责分层：

1. 硬 IRQ 只执行有界的所有权检查、ACK/mask、发布 deferred work，并调用
   `IrqNotify::notify_irq()`；
2. 固定的 `usbfs-event-worker` 在任务上下文中 drain controller event；
3. command 与 transfer queue 使用 Release 内存序发布结果，然后调用已注册的标准
   `Waker`；
4. PR #1775 的 `LocalExecutor` 与 `ThreadWakeHandle` 使可 join 的 `usbfs-init`
   worker 重新进入 runnable 状态；
5. 启动线程 join 该 worker，并消费其类型化报告；报告包含 ready host 数量以及每个
   host 的失败阶段。

queue 注册保持 check/register/check 模式。注册前发生的 wake 由第二次 ready check
观察；注册后发生的 wake 通过已保存的 `Waker` 观察。USB 代码不会直接操作调度器
run queue，也不会保存裸 task 指针。

## 失败回滚

framework action 按 host 事务性启用。缺少 action handle 或 enable 失败时，已启用的
action 按逆序关闭；随后停用各 event gate，并等待所有 active handler 退出。

controller 所有权建立后的回滚顺序为：

```text
mask controller
  -> disable framework action
  -> deactivate event gate
  -> wait for handler quiescence
  -> free framework action
```

- controller prepare 失败时，中断源保持 masked；
- root hub 初始化失败时，由 `Core` 恰好回滚一次；同时保留 root hub backend，供显式
  retry 使用；
- initial probe 失败时，Starry 先 mask controller，再 disable action；
- reacquire 或 lock 失败发生在 action 激活之前，因此可以直接释放仍处于 disabled
  状态的已注册 action。

持有 controller-wide 或 manager-wide lock 时不会调用 callback。event gate 的
active、busy 和 deferred 位共用一个 `AtomicU8`；状态通过 Release 发布，并通过
Acquire/AcqRel 取得。device/topology dirty 状态在通知任务所有的 refresh 路径之前
发布。

## 回归与验证证据

修复前，issue 原命令运行到 `kmod subsystem initialized` 后，在共享 INTx xHCI
设备存在的情况下连续 90 秒无输出。确定性单元回归同时观测到：

- `Core` 的调用顺序为 `prepare -> root hub init`，缺少 enable 转换；
- root hub 失败时没有调用 controller disable；
- xHCI prepare 得到的 `IMAN` 值中 `IE=1`；
- Starry 先初始化 host 并从外部 enable controller，之后才 enable framework
  action。

修复后的测试直接编译真实实现，并覆盖：

- `prepare -> enable -> root hub init` 以及 root hub 失败时恰好一次回滚；
- xHCI prepare masked、RW1C pending/EHB 保留以及 ACK/rearm 竞争；
- DWC2 masked prepare；
- framework action 部分 enable 失败的回滚；
- action-before-init 顺序，以及 probe 失败时 controller-before-framework 的回滚；
- `crab-usb` 与 `ax-task` 既有测试负责的 wake-before-register 和
  register-before-wake 行为。

端到端 QEMU 结果只在 runner 输出 `STARRY_GROUPED_TESTS_PASSED` 等终态成功标记时
记录为通过。仅启动、pending、cancelled 或人工中断的运行均不作为通过证据。

修复后，issue 原命令在 AArch64 上进入用户态并输出
`STARRY_GROUPED_TESTS_PASSED`。完整 AArch64 `qemu/system` 聚合测试也以 400/400
通过并输出相同终态标记，其中 USB audio ISO 与 USB mass storage 用例均通过。
