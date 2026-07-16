# `axpoll`

> 路径：`components/axpoll`
> 类型：库 crate
> 分层：组件层 / 通用 readiness 与唤醒协议层
> 版本：`0.5.1`
> 文档依据：`Cargo.toml`、`src/lib.rs`、`tests/tests.rs`、`tests/async.rs`、`net/ax-net/src/blocking.rs`、`os/StarryOS/kernel/src/task/future.rs`

`axpoll` 为仓库里的“对象可轮询事件”提供了一套极小但很关键的公共协议：用 `IoEvents` 表示事件位，用 `Pollable` 约定对象如何报告就绪状态和注册 waker，用 `PollSet` 保存等待者并在状态变化时唤醒。网络 socket、文件节点、loopback 设备、IRQ 等对象都可以接到这套模型上。

最关键的一条边界是：`axpoll` 只提供 readiness 与唤醒协议，不是 `poll(2)` / `epoll(7)` 的系统调用实现，更不是调度器。

## 架构设计

### 设计定位

仓库里已经有 `axio` 负责同步读写语义，但还需要另一层来回答两个问题：

- 这个对象“现在”有哪些事件已经成立？
- 如果事件还没成立，应该把谁记下来，等状态变化时再唤醒？

`axpoll` 正是为这两件事存在的。它位于：

- `axio` 之上：`axio` 只管同步 I/O 接口，不管等待
- 调度桥接层之下：`ax-net` 的同步桥接和 StarryOS 的线程本地 future 层消费 `Pollable`，`axpoll` 本身不依赖调度器
- ArceOS/StarryOS 多路复用实现之下：更高层 `select` / `poll` / `epoll` 轮询的对象，底层往往实现 `Pollable`

### 1.2 单文件核心结构

虽然 crate 只有一个 `src/lib.rs`，内部职责很清晰：

| 组成 | 作用 |
| --- | --- |
| `IoEvents` | 基于 `bitflags` 封装 Linux `POLL*` 事件位 |
| `Pollable` | 约定对象如何查询当前事件，以及如何注册等待者 |
| `Inner` | `PollSet` 的内部 ring buffer，保存 `Waker` 与订阅的事件位 |
| `PollSet` | 对外暴露的等待者集合，可注册与批量唤醒 |

### 1.3 `IoEvents`：readiness 位图协议

`IoEvents` 基本直接对齐 Linux `poll` 语义，包括：

- `IN`、`OUT`
- `PRI`
- `ERR`、`HUP`、`NVAL`
- `RDNORM`、`RDBAND`、`WRNORM`、`WRBAND`
- `MSG`、`REMOVE`、`RDHUP`

其中 `ALWAYS_POLL` 把 `ERR` 与 `HUP` 固定为“即使未显式订阅也应参与判断”的事件位。这一设计使内核对象 readiness 与 POSIX 兼容层的事件语义能够共享同一套位图定义。

### 1.4 `PollSet` 的真实实现约束

`PollSet` 看起来像一个等待者集合，但它不是无界队列，而是一个固定容量为 64 的 ring buffer。当前实现有几条必须写进文档的行为约束：

- `register()` 会把 waker 和它订阅的 `IoEvents` 写入循环缓冲区
- 超过 64 个等待者后，新注册会覆盖最旧槽位
- 被覆盖掉的旧 waker 若与新 waker 不是同一个，会被立即唤醒
- `wake(ready)` 只唤醒订阅事件与 `ready` 相交的等待者，未就绪的等待者会留在集合中
- `wake_from_irq(ready)` 面向设备中断路径，不分配新的等待队列存储，也同样按事件位过滤
- `PollSet` 自身 `Drop` 时会再触发一次 `wake(IoEvents::all())`，避免等待者永远悬挂

因此，`PollSet` 的真实语义更接近“有限容量的唤醒集合”，而不是严格意义上的公平等待队列。

### 1.5 与调度器的桥接关系

仓库现在有两种显式桥接，而不是把 future executor 放进 `ax-task` 的 I/O
模块中：

1. `net/ax-net/src/blocking.rs` 为同步 socket 调用创建绑定当前线程的
   `ThreadWakeHandle`，按“操作、注册、再次操作、park”顺序关闭丢唤醒窗口。
2. `os/StarryOS/kernel/src/task/future.rs` 在调用线程上 poll future；它使用
   `ax-std` 转导的 runtime scheduler facade 创建 waker，并在 `poll_io()` 中注册
   `Pollable`。
3. 两条路径都要求上层提供一个返回 `AxError::WouldBlock` 的 nonblocking 操作，
   readiness 成立后再重试操作。

`PollSet::wake_from_irq()` 自身不扩容，但它最终会执行已注册的 `Waker`。
因此只有所有 waker 都满足 hard-IRQ-safe 契约时才能直接调用。当前网络和 Starry
生产路径把 IRQ 先合并到固定 service thread，再由普通任务上下文执行
`PollSet` fan-out，避免在中断里运行任意 waker。

## 核心功能

### 功能概览

- 用统一位图表达可读、可写、挂断、错误等事件
- 为任意内核对象定义 `poll()` / `register()` 契约
- 提供可复用的 `PollSet`，让对象能够保存等待者并在状态变化时批量唤醒
- 保存 Rust `Waker`，让内核对象能在状态变化时接入任务唤醒链路

### 2.2 仓库里的真实使用者

当前仓库中直接依赖 `axpoll` 的关键路径包括：

- `ax-net`：为 TCP、UDP、Unix domain socket、vsock、loopback 设备提供统一 readiness 语义
- `ax-fs-ng` / `axfs-ng-vfs`：为文件节点暴露可轮询事件
- StarryOS 内核：为 `FileLike`、pipe、TTY、socket、eventfd、epoll 等对象复用同一套等待协议，并在 `task::future` 中桥接线程本地执行

### 2.3 `Pollable` 的职责边界

一个对象实现 `Pollable` 时，实际上是在承诺两件事：

- `poll()`：只报告“现在已经成立”的事件位，不做阻塞等待
- `register()`：只保存或转发 waker，不在这里推进行为状态机

也就是说，`Pollable` 描述的是 readiness 协议，不是对象本身的业务逻辑。

### 2.4 关键边界

- `axpoll` 不负责读写语义；那是 `axio` 的职责
- `axpoll` 不负责超时策略；超时由 `ax-net` 阻塞桥接或 StarryOS `task::future` 等上层处理
- `axpoll` 不负责系统调用级 `poll` / `epoll` 数据结构和 fd 管理
- `axpoll` 不替对象生成事件，只消费对象已经判断好的 readiness

## 依赖关系

### 直接依赖

| 依赖 | 作用 |
| --- | --- |
| `ax-kspin` | 为 `PollSet` 内部状态提供始终 SMP-safe 的 IRQ-aware 锁 |
| `bitflags` | 定义 `IoEvents` 位图 |
| `linux-raw-sys` | 复用 Linux `POLL*` 常量值 |
| `spin` | 在 `no_std` 下提供一次性懒初始化 |

`axpoll` 始终是 `no_std + alloc`；当前没有 operational feature 分叉。

### 主要消费者

| 消费者 | 使用方式 |
| --- | --- |
| `ax-net` | 为不同地址族 socket 与设备统一事件位和 waker 注册 |
| `ax-fs-ng` | 让文件节点支持统一 readiness 协议 |
| StarryOS 内核 | 作为 fd readiness glue，并通过线程本地 future 层接入 scheduler facade |

## 开发指南

### 4.1 依赖方式

```toml
[dependencies]
axpoll = { workspace = true }
```

### 4.2 为新对象实现 `Pollable` 的建议

1. `poll()` 中只读当前状态，不要在这里阻塞或睡眠。
2. `register()` 中只保存/转发 waker；真正的 `wake()` 必须发生在状态变化点。
3. 如果对象有不同类型的唤醒源，优先按读、写、关闭、异常分开组织 `PollSet`。
4. 如果对象有 IRQ 来源，优先参考 `ax-net` 和 Starry `IrqNotify` 的固定 service-thread 模式；只有 waker 契约明确 hard-IRQ-safe 时才直接调用 `wake_from_irq()`。

### 4.3 修改实现时的风险点

- `PollSet` 的 64 项容量是实现边界，不可误当成无限等待列表
- 覆盖旧 waker 时会主动唤醒旧者，这会影响高并发下的重试频率和公平性
- `wake(ready)` / `wake_from_irq(ready)` 必须先从集合里移除已就绪等待者，再在释放锁后执行 waker，改这条路径极易产生丢唤醒或重入死锁
- `IoEvents` 与 Linux 常量必须保持稳定对应关系，否则上层兼容性会直接出问题

## 测试

### 5.1 当前已有测试

`components/axpoll/tests` 已覆盖两个关键方向：

- `tests.rs`：验证注册、空唤醒、满容量、覆盖旧 waker、drop 时唤醒
- `async.rs`：用 `tokio` future 验证单任务与多任务的等待/唤醒链路

### 5.2 建议重点

- 注册后立即就绪时是否仍能正确返回
- 超容量覆盖时旧 waker 是否被唤醒
- 对象或 `PollSet` 被销毁时是否留下悬挂等待者
- 任何 `Pollable` 语义调整都应补 `ax-net` 同步桥接或 Starry `task::future::poll_io` 的集成验证

### 5.3 推荐验证命令

```bash
cargo test -p axpoll
```

## 跨项目定位

### ArceOS

在 ArceOS 中，`axpoll` 是 `ax-net`、`ax-fs-ng` 与上层任务等待之间的公共
readiness glue；调度和 park 由 `ax-task` facade 的消费者显式完成，而不是由
`axpoll` 反向依赖调度器。

### StarryOS

在 StarryOS 中，`axpoll` 的地位更直接。大量 `FileLike` 对象、pipe、socket 与 `epoll` 相关路径都建立在这套 readiness 协议之上。

### Axvisor

当前没有看到 Axvisor 直接把 `axpoll` 当作独立子系统来消费的证据。它即使间接复用，也更可能经过 ArceOS/Starry 的公共层，而不是作为 hypervisor 侧专门框架存在。
