# ax-task 命名空间与所有权

## 1. 公开接口

ax-task 只在根目录公开领域模块。ArceOS 使用 `pub use ax_task as task`，因此 `ax_runtime::task` 与 ax-task 是同一软件包的接口，不维护第二份类型列表或调度实现。运行时与调度器之间仍使用一份 `TaskRuntime` 能力表。

### 1.1 调用方入口

普通使用者按领域选择入口；集成内核的调用方才使用 `runtime` 能力。`lib.rs` 不提供平铺类型、通配转导出或兼容别名。

| 入口 | 所有者与职责 |
| --- | --- |
| `task::thread` | `ThreadHandle`、线程身份、生命周期和可移植 `ThreadBuilder` |
| `task::thread::current` | 当前线程查询、park、yield、退出与同步亲和性迁移 |
| `task::sched` | 调度策略、优先级、CPU 集合、亲和性完成令牌与运行记账结果 |
| `task::sync` | 唯一的 PI `Mutex` 实现、非睡眠锁与等待队列 |
| `task::sync::irq` | IRQ 通知单元、登记令牌与固定所有者等待 |
| `task::sync::membarrier` | 地址空间登记及调度器参与的内存屏障 |
| `task::time` | `MonotonicInstant` 与 `MonotonicDeadline` |
| `task::time::timer` | 普通回调登记、重启和公共取消结果 |
| `task::time::hard_timer` | 明确承诺 IRQ 安全的回调及可重复 arm/disarm 句柄 |
| `task::executor` | `LocalExecutor`、`block_on` 与带超时的 Future 驱动 |
| `task::diagnostics` | 功能开关控制的计数与调度诊断 |

`ThreadHandle::lookup` 负责从代际 ID 获取管理租约。已有句柄直接调用 `base_policy`、`runtime`、`set_policy`、`affinity`、`request_affinity` 或 `set_affinity_and_wait`；不再同时提供同义自由函数。需要多次操作时复用句柄，避免重复登记表查询。

### 1.2 内核集成

`runtime::TaskSystem` 是显式调度对象，配置集中在 `runtime::config`。其余集成类型按 CPU、资源、切换、服务和同步桥接分组，普通线程代码无需直接访问运行队列。

| 集成模块 | 契约 |
| --- | --- |
| `runtime::cpu` | `CpuLocal`、`CpuRemote`、成对 CPU 句柄、IRQ/抢占令牌及 clockevent 观测 |
| `runtime::resource` | 栈、TLS、执行上下文、地址空间所有权及回收协议 |
| `runtime::switch` | `RuntimeSwitchPlan`、调度进入来源、continuation 返回条件及切换完成 |
| `runtime::service` | tick、task deadline、soft timer 与延迟回收服务 |
| `runtime::sync` | OS 锁适配需要的存储视图与 PI/等待协议 |

`ax_runtime::thread` 只提供 ArceOS 资源装配、地址空间绑定、OS 扩展和线程创建发布。物理 IPI 通知及固定 IRQ worker 信号归入 `ax_runtime::irq`，OS 切换跟踪和计时诊断归入 `ax_runtime::diagnostics`。Future 驱动使用既有 `TaskRuntime` 时钟，归入 ax-task，避免在运行时重复实现。

## 2. 内部实现

目录按状态所有者组织。`sched::algorithm` 保存纯调度算法，`sched::system` 保存 CPU 运行队列和调度事务；`runtime::delivery` 保存逻辑消息与回收工作投递；`time::queue` 保存任务 deadline 和 callback 队列。私有实现不构成另一套公开入口。

### 2.1 事务边界

运行队列拆成成员维护、当前任务、运行记账、Deadline、idle 与抢占判断。`OwnerRqTxn` 的观测、成员操作、选择、记账和提交分别成模块，但仍由原来的一个事务对象持锁。模块拆分不提前释放锁、不复制状态，也不改变发布或析构顺序。

`TaskSystem` 的调度选择、请求抢占、yield、park、退出、切换完成和唤醒投递按阶段拆分。唤醒来源通过原有所有者激活路径提交；本地与远端仅保留其上下文和传输契约必要的差异。切换尾部共用 `finish_switch_tail`，调用方提供已经取得的 `TaskSystem` 与 CPU pin，不增加运行时查询。

### 2.2 资源与复用

`ThreadBuilder` 自身保存配置及 OS 扩展析构责任，删除并行的 `KernelThreadSpec` 配置入口与公开 spawn 自由函数。PI 存储容器 `PiMutexCore` 只负责初始化、借出视图和析构，获取、释放及交接算法统一在 `PiMutexCoreView`，删除逐项转发方法。

hard/soft timer 共用 `time::queue::kernel` 的登记、取消和队列所有权。hard callback 的能力类型、执行上下文和延迟析构边界继续独立。`KernelTimerCancelOutcome::CancellationDeferred` 不被折叠为成功布尔值，`AlreadyCompleted` 也不是资源回收栅栏。

调度与内存分配相关路径不新增 `Box`、`Vec` 或 `Arc` 分配。队列容量仍在初始化或任务发布前准备，临界区不新增最后一个引用的释放；回调及资源最终析构继续由任务上下文负责。模块拆分不引入动态分派、第二份登记表或状态缓存。

### 2.3 保持整体的算法

行数用于触发审查，而不决定状态机边界。本轮将混合职责的大文件拆开；以下局部算法仍保持整体，避免将紧密协作的索引修复分散到多个接口。

| 文件 | 保留理由 |
| --- | --- |
| `sched/algorithm/fair_queue.rs` | EEVDF 有序树的插入、删除、旋转和增强值更新共同维护同一个树不变量 |
| `sched/algorithm/queue/realtime.rs` | RT 优先级队列、当前任务链接及 pushable 索引在同一队列所有权下更新 |
| `sync/lockdep/state.rs` | 有界锁类登记、依赖图和 held-lock 快照共同完成一次锁序判定，没有拆出第二份锁状态 |

这些文件继续使用私有字段和原有同步边界。若后续修改引入独立生命周期或独立失败策略，应再按那个边界拆分，而不是增加跨模块可变字段访问。

## 3. 行为验证

命名空间迁移使用真实下游构建来验证可见性与功能组合。功能测试保护所有权和可观察行为，不检查文件名、源码格式或导入文本。

### 3.1 功能场景

`task-executor` 通过公开接口验证零超时下的就绪结果、超时取消后的 Future 析构以及另一线程的 waker 通知。它取代原运行时模块内的单个边界断言，纳入 Cargo feature、测试 runner、用例选择及 axbuild 发现列表，避免移动后失去执行入口。

已有 park/唤醒、PI、亲和性、timer 取消与 AxVM timer 用例继续覆盖真实 QEMU 运行。Loom 和纯算法测试保留各自负责的交错或队列不变量；模块拆分不以伪 `TaskRuntime` 替代内核调度验证。

### 3.2 性能约束

重构后在同一台 OrangePi 上配对运行最新 dev 与 PR 分支，覆盖全部 20 项调度/唤醒场景（10 项 × OTHER/FIFO80）。使用相同构建配置、DTB、CPU 亲和性和冻结基准二进制，多轮核对实际样本数、结果数量及退出状态，报告原始分位数与扣除取时成本后的辅助值。延迟性能百分比使用参考 p50 除以 PR p50，避免把延迟增长误写成性能提升。

Linux RT 只引用议题 [#2308](https://github.com/rcore-os/tgoskits/issues/2308) 保存的 OrangePi 历史基线，不重复测量。PR 正文记录两个实际被测提交、板卡与构建信息、逐项 PR/dev 和 PR/Linux RT 百分比及可比性限制；接口与模块收敛本身不代表完成 Linux RT 性能追赶。
