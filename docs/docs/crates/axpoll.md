# `axpoll` 技术文档

> 路径：`components/axpoll`
> 类型：库 crate
> 分层：组件层 / 通用 readiness 与唤醒协议层
> 版本：`0.1.2`
> 文档依据：`Cargo.toml`、`src/lib.rs`、`tests/tests.rs`、`tests/async.rs`、`os/arceos/modules/axtask/src/future/poll.rs`

`axpoll` 为仓库里的“对象可轮询事件”提供了一套极小但很关键的公共协议：用 `IoEvents` 表示事件位，用 `Pollable` 约定对象如何报告就绪状态和注册 waker，用 `PollSet` 保存等待者并在状态变化时唤醒。网络 socket、文件节点、loopback 设备、IRQ 等对象都可以接到这套模型上。

最关键的一条边界是：`axpoll` 只提供 readiness 与唤醒协议，不是 `poll(2)` / `epoll(7)` 的系统调用实现，更不是调度器。

## 1. 架构设计分析

### 1.1 设计定位

仓库里已经有 `axio` 负责同步读写语义，但还需要另一层来回答两个问题：

- 这个对象“现在”有哪些事件已经成立？
- 如果事件还没成立，应该把谁记下来，等状态变化时再唤醒？

`axpoll` 正是为这两件事存在的。它位于：

- `axio` 之上：`axio` 只管同步 I/O 接口，不管等待
- `ax-task` future 机制之下：`ax-task::future::poll_io` 依赖 `Pollable`
- ArceOS/StarryOS 多路复用实现之下：更高层 `select` / `poll` / `epoll` 轮询的对象，底层往往实现 `Pollable`

### 1.2 单文件核心结构

虽然 crate 只有一个 `src/lib.rs`，内部职责很清晰：

| 组成 | 作用 |
| --- | --- |
| `IoEvents` | 基于 `bitflags` 封装 Linux `POLL*` 事件位 |
| `Pollable` | 约定对象如何查询当前事件，以及如何注册等待者 |
| `Inner` | `PollSet` 的内部 ring buffer，保存 `Waker` |
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

- `register()` 会把 waker 写入循环缓冲区
- 超过 64 个等待者后，新注册会覆盖最旧槽位
- 被覆盖掉的旧 waker 若与新 waker 不是同一个，会被立即唤醒
- `wake()` 会把旧 `Inner` 整体换出，然后依靠旧 `Inner` 的 `Drop` 逐个唤醒
- `PollSet` 自身 `Drop` 时会再触发一次 `wake()`，避免等待者永远悬挂

因此，`PollSet` 的真实语义更接近“有限容量的唤醒集合”，而不是严格意义上的公平等待队列。

### 1.5 与 `ax-task` 的桥接关系

`os/arceos/modules/axtask/src/future/poll.rs` 展示了 `axpoll` 在系统里的标准用法：

1. 上层提供一个同步 nonblocking I/O 闭包，并在暂不可完成时返回 `AxError::WouldBlock`
2. `poll_io()` 先执行该闭包
3. 若返回 `WouldBlock` 且非 nonblocking 模式，则调用 `pollable.register(cx, events)`
4. 事件成立后，由对象自身通过 `PollSet::wake()` 或自定义注册逻辑唤醒等待任务

同一文件里还有 `register_irq_waker()`，说明 `axpoll` 不只服务文件/网络对象，也被用来桥接 IRQ 事件与任务等待。

## 2. 核心功能说明

### 2.1 主要能力

- 用统一位图表达可读、可写、挂断、错误等事件
- 为任意内核对象定义 `poll()` / `register()` 契约
- 提供可复用的 `PollSet`，让对象能够保存等待者并在状态变化时批量唤醒
- 通过实现 `Wake`，让 `PollSet` 能直接接入 Rust waker 生态

### 2.2 仓库里的真实使用者

当前仓库中直接依赖 `axpoll` 的关键路径包括：

- `ax-task`：把同步 nonblocking I/O 封装成可等待 future
- `ax-net-ng`：为 TCP、UDP、Unix domain socket、vsock、loopback 设备提供统一 readiness 语义
- `ax-fs-ng` / `axfs-ng-vfs`：为文件节点暴露可轮询事件
- StarryOS 内核：为 `FileLike`、pipe、TTY、socket、eventfd、epoll 等对象复用同一套等待协议

### 2.3 `Pollable` 的职责边界

一个对象实现 `Pollable` 时，实际上是在承诺两件事：

- `poll()`：只报告“现在已经成立”的事件位，不做阻塞等待
- `register()`：只保存或转发 waker，不在这里推进行为状态机

也就是说，`Pollable` 描述的是 readiness 协议，不是对象本身的业务逻辑。

### 2.4 关键边界

- `axpoll` 不负责读写语义；那是 `axio` 的职责
- `axpoll` 不负责超时策略；超时通常由 `ax-task::future::timeout` 或上层 socket 层处理
- `axpoll` 不负责系统调用级 `poll` / `epoll` 数据结构和 fd 管理
- `axpoll` 不替对象生成事件，只消费对象已经判断好的 readiness

## 3. 依赖关系

### 3.1 直接依赖

| 依赖 | 作用 |
| --- | --- |
| `bitflags` | 定义 `IoEvents` 位图 |
| `linux-raw-sys` | 复用 Linux `POLL*` 常量值 |
| `spin` | 在 `no_std` 下为 `PollSet` 提供轻量锁与懒初始化 |

`Cargo.toml` 中的 `alloc` feature 已标记为 deprecated，目前更多是兼容旧用法，而不是新设计中的能力分层。

### 3.2 主要消费者

| 消费者 | 使用方式 |
| --- | --- |
| `ax-task` | 通过 `poll_io()`、IRQ waker 等机制消费 `Pollable` 与 `PollSet` |
| `ax-net-ng` | 为不同地址族 socket 与设备统一事件位和 waker 注册 |
| `ax-fs-ng` | 让文件节点支持统一 readiness 协议 |
| StarryOS 内核 | 作为 fd 世界底层的 readiness glue 层 |

## 4. 开发指南

### 4.1 依赖方式

```toml
[dependencies]
axpoll = { workspace = true }
```

### 4.2 为新对象实现 `Pollable` 的建议

1. `poll()` 中只读当前状态，不要在这里阻塞或睡眠。
2. `register()` 中只保存/转发 waker；真正的 `wake()` 必须发生在状态变化点。
3. 如果对象有不同类型的唤醒源，优先按读、写、关闭、异常分开组织 `PollSet`。
4. 如果对象本身有 IRQ 或设备事件来源，可参考 `ax-task` 与 `ax-net-ng` 的做法，把底层事件桥接到 `PollSet`。

### 4.3 修改实现时的风险点

- `PollSet` 的 64 项容量是实现边界，不可误当成无限等待列表
- 覆盖旧 waker 时会主动唤醒旧者，这会影响高并发下的重试频率和公平性
- `wake()` 依赖旧 `Inner` 的 `Drop` 完成真正逐个唤醒，改这条路径极易产生丢唤醒
- `IoEvents` 与 Linux 常量必须保持稳定对应关系，否则上层兼容性会直接出问题

## 5. 测试策略

### 5.1 当前已有测试

`components/axpoll/tests` 已覆盖两个关键方向：

- `tests.rs`：验证注册、空唤醒、满容量、覆盖旧 waker、drop 时唤醒
- `async.rs`：用 `tokio` future 验证单任务与多任务的等待/唤醒链路

### 5.2 建议重点

- 注册后立即就绪时是否仍能正确返回
- 超容量覆盖时旧 waker 是否被唤醒
- 对象或 `PollSet` 被销毁时是否留下悬挂等待者
- 任何 `Pollable` 语义调整都应补一条 `ax-task::future::poll_io` 集成验证

### 5.3 推荐验证命令

```bash
cargo test -p axpoll
```

## 6. 跨项目定位

### 6.1 ArceOS

在 ArceOS 中，`axpoll` 处在同步 nonblocking I/O 与任务等待之间，是 `ax-task`、`ax-net-ng`、`ax-fs-ng` 的公共 readiness glue 层。

### 6.2 StarryOS

在 StarryOS 中，`axpoll` 的地位更直接。大量 `FileLike` 对象、pipe、socket 与 `epoll` 相关路径都建立在这套 readiness 协议之上。

### 6.3 Axvisor

当前没有看到 Axvisor 直接把 `axpoll` 当作独立子系统来消费的证据。它即使间接复用，也更可能经过 ArceOS/Starry 的公共层，而不是作为 hypervisor 侧专门框架存在。
