---
sidebar_position: 7
sidebar_label: "锁与并发"
---

# 锁与并发

`ax-net` 的并发设计不是“给完整网卡对象加一把大锁”，而是把执行权拆成三个互不
重入的 owner：hard IRQ endpoint、fixed-CPU queue executor、唯一 protocol executor。
跨 owner 只发布原子状态、generation 或 move-only token。

详细状态机见[队列级 NAPI 运行时](queue-napi-runtime.md)，设备数据路径见
[多设备实现](devices.md)。

## 1. Execution owner

| 执行域 | 独占对象 | 可以进入 | 禁止进入 |
| --- | --- | --- | --- |
| hard IRQ callback | move-only `NetHardIrqEndpoint` | bounded mask/ack/status snapshot，group atomic publish | DMA payload、allocation、sleep lock、smoltcp、任意 waker、transport spin |
| `net-queue-cpuN` | 该 CPU 的 `NetPollGroup` queue/control endpoints | DMA reclaim/refill、TX submit/completion、budget、backpressure、rearm、Wi-Fi transaction | `Service`、`SocketSet`、其它 CPU 的 queue |
| `net-protocol` | `Service`、smoltcp `Interface`、全局 `SocketSet`、Router、DHCP | socket protocol、route、ARP frame port、readiness wake | hardware queue/MMIO/SDIO |
| socket caller | 自身 socket state 与 wait object | mutate socket 后 `request_poll()`，等待 readiness/generation | 直接执行 smoltcp poll、直接访问 queue/control endpoint |

硬性 CPU 不变量：

```text
IRQ callback CPU == queue processing CPU == group owner_cpu
```

只有 frame/DMA token pipeline 可以让 queue owner 与 protocol owner 位于不同 CPU；
这不是 IRQ continuation。

## 2. Protocol lock order

唯一 protocol executor 进入共享状态时遵循：

```text
SERVICE (Mutex<Service>)
  -> SOCKET_SET.inner (Mutex<SocketSet>)
    -> TCP_BOUND_PORTS
      -> LISTEN_TABLE bucket
  -> NET_CONTROL.state / shared RouteTable
```

- `SERVICE` 保护 smoltcp `Interface`、DHCP client/server 和 Router 调度。
- `SOCKET_SET.inner` 保护全局 smoltcp sockets。
- `ListenTable` 条目只能沿 `SOCKET_SET` 向下获取。
- `NetControl` 查询返回快照，不把 lock guard 暴露到 ABI 层。
- queue executor 与 hard IRQ 永远不进入这条链。

Unix domain socket 与 vsock 具有各自 transport/connection lock，但不能在持有其局部
lock 时反向调用完整 IP protocol poll。

## 3. Protocol generation

`ProtocolPollRuntime` 包含：

```text
requested: AtomicU64
completed: AtomicU64
scheduled: AtomicBool
worker_wake: WaitQueue
completion: WaitQueue
```

调用者 `fetch_add` requested 后只在 `scheduled false -> true` 时通知 worker。唯一
worker 记录 target、poll until idle、Release publish completed。结束一轮时先清
scheduled，再 Acquire 比较 requested/completed；若 request 在 completion 窗口并发，
立即保留 scheduled 并继续。

`flush_egress()` 等同步路径等待自己的 generation，不能取得临时 protocol ownership。
wrapping 比较使用半区间规则，旧 completion 不能满足未来 generation。

## 4. Poll-group atomic state

state byte 的低位是：

```text
IDLE / SCHEDULED / POLLING / DISABLED
```

高位 `MISSED` 表示已经 scheduled/polling 时又有事件。关键转移：

- publish：`IDLE -> SCHEDULED` 并 notify；`SCHEDULED|POLLING -> same|MISSED`；
- claim：owner CPU CAS `SCHEDULED -> POLLING`；
- more：CAS `POLLING|MISSED -> SCHEDULED`；
- completion：CAS `POLLING -> IDLE`；观察 `MISSED` 时回 `SCHEDULED`；
- disable：任意状态 Release store `DISABLED`。

completion 不能无条件 store，否则会把 concurrent `DISABLED` 复活。当前
`finish_more()` 与 `begin_rearm()` 都使用 CAS，并有穷举顺序测试。

## 5. IRQ callback ownership

driver `into_parts()` 把每个 hard endpoint move 给 registration closure。IRQ framework
以 non-reentrant action 持有 endpoint，free lease 前必须 disable + synchronize，因此
不存在 task 侧同时借用 handler 的路径。

共享 destructive status 由 hard endpoint 唯一读取。它把 snapshot/pending bit 写入
预分配原子状态，queue endpoint 不在 IRQ 中拿自身 task lock。virtio transport gate
竞争返回 `ProbeDeferred`；hard IRQ 不等待被自己抢占的 task owner。

callback 只调用 group-local `IrqNotify::notify_irq()`。physical `IrqId` 的全部 shared
action 必须使用同一 fixed CPU，避免 callback 在 CPU A 发布、queue 在 CPU B 继续。

## 6. Queue ownership 与 SPSC

每个 `QueueGroupExecutor` 独占 `IRxQueue`、`ITxQueue`、`NetPollIrqControl` 和四个
pending token slot。它不需要外部 queue mutex。

queue/protocol 边界是四条单生产者单消费者 ring：

```text
queue -> protocol: RX-ready, TX-free
protocol -> queue: RX-recycle, TX-ready
```

ring slot 使用 `UnsafeCell<MaybeUninit<T>>`；producer 写 slot 后 Release tail，consumer
Acquire tail 后读取；consumer Release head 后 producer 才能复用。producer/consumer
endpoint 通过 `PhantomData<Cell<()>>` 保持非 `Sync`，确保每侧只有一个 owner。

`T: Send` 是跨 CPU 条件。`DmaBuffer` move-only，失败返回原 token，所以 ring full、
driver retry、executor stop 都不会产生两个可变别名。

## 7. Budget 与 blocked state

owner 在 IRQ mask 状态按 RX recycle、RX reclaim、TX completion、TX submit 各 64 的
预算推进，每 CPU round 256。预算用尽只把 group 重新置为 scheduled，不重开 IRQ。

ring full 时 group 留在 `POLLING`，token 保存在 `pending_*`。protocol owner 释放空间
后通过 `schedule_task()` 设置 `MISSED` 并精准通知 owner。没有 busy loop、设备 timer、
全局 fanout 或依赖“下一个 IRQ 恰好到来”的隐式恢复。

## 8. Rearm ordering

正常 completion 顺序：

```text
drain queues
  -> CAS POLLING -> IDLE
  -> device/controller rearm_and_check()
  -> pending ? mask + schedule : remain IDLE
```

若 IRQ 在 CAS 前到达，publish 设置 `MISSED`，completion 回到 `SCHEDULED`。若在 CAS
后、rearm 中到达，hardware pending check 返回 `WorkPending`。若在 rearm status check
之后到达，source 已 unmask，fixed-CPU callback 正常发布。

SDHCI CARD_INT 的 task rearm 将 unmask 和 controller status readback 放在同一方法；
读到 pending 时立刻重新 mask。AIC chip-side pending/FIFO 也在返回 idle 前复查。

## 9. Wi-Fi quiesce transaction

`WifiControlQueue` 使用短 IRQ-safe lock 保护容量 8 的 request deque；completion 使用
独立 `WaitQueue`。调用者只入队 owned transaction。

owner executor 在同一 task 中：

1. `quiesce()` 所属 group；
2. 执行 firmware/SDIO control；
3. `rearm_and_check()`；
4. publish completion。

因此控制路径不会在任意 CPU 直接碰 SDIO/MMIO，也不会和 RX/TX queue owner 并发。
runtime stop 先标记 queue stopped 并完成所有 pending request，再处理 IRQ/worker teardown。

## 10. 初始化与 teardown

初始化先完成所有不会外部可见的资源：parts 校验、DMA pool、SPSC、state、worker。
worker affinity-ready 后才注册 disabled IRQ；initial refill/rearm 成功后 enable；Wi-Fi
startup transaction 和 MAC refresh 成功后才发布 service。

失败与 Drop 顺序：

```text
stop accepting Wi-Fi requests
  -> disable/synchronize IRQ registrations
  -> command executors to stop
  -> join executors
  -> drop queues/pools/control/device resources
```

`disable_and_synchronize()` 先保证没有 in-flight callback，executor stop 再 quiesce
hardware。join 完成后才允许 endpoint/token drop。

## 11. Atomic ordering

- group state、generation publish/observe：Acquire/Release 或 AcqRel CAS；
- `last_irq_cpu`/`last_poll_cpu` publish：Release，统计读取 Acquire；
- 纯计数器：Relaxed，不承担其它状态同步；
- SPSC slot：payload write/read 由 Release/Acquire head/tail 配对；
- DMA CPU/device 可见性：由 buffer sync API 和 driver doorbell/completion ordering 保证，
  不能用普通 Rust atomic 替代 cache maintenance。

## 12. 禁止模式

- network IRQ 使用 `IrqAffinity::Any`；
- IRQ callback remote wake/IPI 到另一个 queue CPU；
- hard IRQ allocation、DMA payload access、smoltcp poll、arbitrary waker 或 gate spin；
- task 调用 raw hard handler、queue 重新读取并清除 shared IRQ status；
- 完整 driver object 的 `Arc<Mutex<_>>` 同时被 IRQ/task/协议层访问；
- per-device RX/TX background task、周期 device poll、wake-all fanout、AIC kicker；
- socket caller 或同步 flush 成为第二 protocol owner；
- queue/control endpoint 在未 quiesce 时跨 CPU 调用；
- 无条件 completion store 复活 `DISABLED`；
- submit error 丢弃 move-only DMA token。

新增并发路径必须说明 owner、锁/CAS 顺序、IRQ mask 域、DMA publish/observe 和 stop
行为，并提供确定性交错测试或目标硬件证据。
