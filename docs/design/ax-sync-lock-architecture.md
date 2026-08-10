# 全项目锁语义统一设计

## 1. 背景与问题

本次修改属于高风险、跨子系统的同步边界重构。修改前，同一个“自旋锁”概念分别由
`ax-kspin`、`ax-kernel-guard`、`ax-sync`、`ax-lockdep` 和 crates.io `spin`
表达。类型名经常把执行上下文固化在类型上，但调用点又无法直接说明本次获取是否需要
关闭中断；可睡眠锁则会随 feature 退化为自旋锁。结果是：

- 驱动、文件系统、网络和内核代码面对多套等价但不完全兼容的 API；
- lockdep 无法观察绕过项目锁的第一方 `spin` 锁；
- `ax-sync` 若直接依赖 task/runtime，会形成同步层到调度层的反向依赖；
- StarryOS 和 Axvisor 很难在 crate 边界上强制自己的锁策略；
- 锁类型名称不能独立回答“是否可睡眠、是否关闭 IRQ、谁负责恢复上下文”。

使用者包括 ArceOS runtime/task、StarryOS kernel、Axvisor、文件系统、网络栈、
可移植驱动、内存管理和虚拟设备。成功标准是第一方代码只有一个公共锁 crate，调用点
能够审计执行上下文，生产构建只有一个 runtime provider，并且既有 ABI、syscall 行为和
自旋读写锁公平性不变。

## 2. 边界与依赖方向

`ax-sync` 是唯一公共锁 crate。它拥有锁算法、guard、可睡眠 mutex 和 lockdep 状态机，
但不拥有硬件或调度器：

```text
ax-task ────────> ax-sync <──────── drivers / fs / net / components
   │                 ▲
   └────> ax-runtime─┘
             │
             ├── ax-hal：IRQ 保存、关闭和恢复
             └── ax-task::WaitQueue：阻塞、单唤醒和 waiter 生命周期
```

具体规则如下：

- `ax-sync` 只依赖 `ax-crate-interface` 等基础 crate，不依赖 `ax-task` 或
  `ax-runtime`。
- `ax-task` 使用 `ax-sync`，但不提供同步算法。
- `ax-runtime` 是 ArceOS 生产环境唯一的能力 provider。
- 显式启用 `host-test` 时由 `ax-sync` 内置的 std provider 支持，并开放确定性测试探针；
  不能用 target triple 的 `target_os` 推断 provider，因为 ArceOS 的 std 兼容目标仍是
  生产内核。其他 OS 必须提供自己的 provider。
- StarryOS kernel 只从 `crate::sync` 导入锁。该 facade 收口 task scope、kprobe、
  namespace 和 POSIX 所需的特殊 adapter。
- Axvisor/AxVM 的普通任务状态使用 `std::sync`；IRQ、guest-entry 或禁止抢占路径只从
  `ax_std::os::arceos::sync` 获取 ArceOS 特有锁，AxVM 不直接依赖 `ax-sync`。
- `ax-kspin`、`ax-kernel-guard`、`ax-lockdep` 和第一方 crates.io `spin` 依赖全部删除。
  `OnceLock`、`LazyLock` 由 `ax-lazyinit` 提供；std 组件使用 `std::sync`。

## 3. 公共锁语义

### 3.1 `SpinLock<T>`

锁对象不固化上下文策略，获取方法表达本次调用的约束：

| 获取方法 | 进入动作 | 退出动作 | 使用场景 |
|---|---|---|---|
| `lock()` / `try_lock()` | 禁止内核抢占 | 恢复抢占深度 | 短临界区，不会在 IRQ handler 重入 |
| `lock_irqsave()` / `try_lock_irqsave()` | 先禁止抢占，再保存并关闭本地 IRQ | 先恢复 IRQ，再恢复抢占 | IRQ 与任务共享状态、scheduler-sensitive 状态 |
| `unsafe lock_raw()` / `try_lock_raw()` | 不改变上下文 | 不改变上下文 | 调用方已证明无同 CPU 重入或外层已完成保护 |

三类 guard 具有不同类型，不能混淆释放策略。raw 获取是 `unsafe`，因为在 UP 构建中
原子锁字可能被裁掉；调用方仍必须证明独占性。`SpinRwLock<T>` 对 read/write 提供相同
三类获取方法。

读写锁继续使用原有非公平算法，不增加 writer preference。公开的瞬时 reader/writer
计数和未使用的强制写解锁被删除；Starry task scope 需要的“释放一个已泄漏读 guard”
保留为 `#[doc(hidden)] unsafe` 接口，并只经专用 wrapper 使用。

### 3.2 `Mutex<T>`

`Mutex<T>` 永远表示可睡眠、无 poisoning 的 mutex，仅在 `sleep` feature 下存在。
非 multitask ArceOS/AXLibC 需要的锁由其 OS facade 显式选择 `SpinLock`，不能改变
`Mutex` 的语义。

状态由非零 task owner ID 和惰性 wait handle 组成：

1. uncontended 获取以 Acquire CAS 将 owner 从 0 改为当前 task ID；
2. 首次竞争时 runtime 分配地址稳定的 `WaitQueue`，用 CAS 安装，失败者释放自己的候选；
3. waiter 在 runtime 内完成“检查 owner—登记等待—睡眠”的原子协议；
4. unlock 先以 Release 发布 owner=0，再唤醒至多一个 waiter；
5. drop 仅在 owner 为 0、没有活动 waiter 时释放已安装队列。

`try_lock` 不调用 `might_sleep`、不分配 wait queue、也不进入调度器。递归获取和错误
owner 解锁会被诊断。POSIX pthread 因 C ABI 不能保存 Rust guard，使用专用 wrapper
泄漏 guard，再调用隐藏的 `unsafe force_unlock`；该接口仍检查当前 task 是 owner。

### 3.3 Guard 和恢复顺序

`PreemptGuard`、`IrqSaveGuard`、`PreemptIrqSaveGuard` 取代独立 guard crate。
组合 guard 的固定顺序是：

```text
acquire: disable_preempt -> irq_save_and_disable
release: irq_restore -> enable_preempt
```

逆序恢复保证 IRQ handler 不会在抢占已经恢复、临界区仍未完全退出时观察中间状态。
IRQ state 是逐次保存的，因此嵌套 IRQ-save guard 不会错误地提前打开中断。

## 4. Runtime 能力接口

`ax-sync` 通过三个最小接口请求外部能力：

- `CriticalSectionOps`：`disable_preempt`、`enable_preempt`、
  `irq_save_and_disable`、`irq_restore`；
- `MutexRuntimeOps`：`might_sleep`、当前 task ID、等待 owner 释放、单个唤醒和
  wait handle 释放；
- `LockdepOps`：IRQ-safe 图状态访问、当前任务 held-lock 快照、push/pop、console 和
  fatal 诊断。

ArceOS 的实现位于 `ax-runtime/src/sync.rs`。`lock-lint` 要求三个生产 provider 在该
文件中各出现一次，并要求生产与 host provider 分别由 `not(feature = "host-test")` 和
`feature = "host-test"` 选择。当前仓库中的其他生产 OS 不能另行注册 provider；若未来
新增独立 runtime，必须同时扩展构建边界和 lint 规则。

## 5. Lockdep 内聚

原 `ax-lockdep` 的 lock class、subclass、依赖图、held-lock stack 和 trace buffer 全部
进入 `ax-sync`。spin mutex、spin rwlock 和 sleep mutex 在同一张依赖图中记录：

- spin 类获取标记 `sleep_forbidden=true`；
- sleep mutex 标记 `sleep_forbidden=false`；
- 获取前检查递归和已有路径反向可达，成功后记录边；
- 释放时校验 held-lock 栈顶与实例地址；
- 动态锁实例共享 class，不按实例消耗 class 槽位。

raw spin 获取仍进入 lockdep trace，但其上下文正确性由 `unsafe` 调用契约保证。
lockdep 关闭时 trace 控制入口是无操作函数，便于 OS facade 保持稳定接口。

## 6. 子系统策略

- `ax-fs-ng`、`ax-net`：可能阻塞、分配或调度的状态用 sleep `Mutex`；IRQ/短临界区用
  `SpinLock::lock_irqsave()`，普通不可睡眠任务临界区用 `lock()`。
- 驱动、内存和虚拟设备：保持 OS 无关，直接使用 `ax-sync`；raw 获取必须在相邻处写明
  无重入和并发排他依据。
- StarryOS：kernel 生产代码不得直接导入 `ax_sync`，特殊适配只存在于
  `crate::sync` 及其明确实现层。用户态 ABI 和 syscall 返回行为不变。
- Axvisor：普通状态使用真实 Rust `std::sync`。只有 IRQ/guest-entry/no-preempt 路径
  使用 `ax_std::os::arceos::sync::{IrqSafeMutex, NoPreemptMutex, RawSpinLock}`。

## 7. Prior art 与方案比较

参考基线为 Linux v6.12：

- `include/linux/spinlock.h` 的普通、`irqsave` 和 raw spin 获取族说明“上下文策略属于
  获取动作”这一设计；
- `kernel/locking/mutex.c` 的 owner 发布、慢路径等待和单 waiter 唤醒说明 sleep mutex
  与 spin mutex 不应由配置静默互换；
- `kernel/locking/lockdep.c` 的 held-lock/class 图模型说明不同锁算法应共享依赖诊断。

评估过的替代方案：

1. 保留 `ax-kspin`，让 `ax-sync` 只重导。它继续暴露两套公共 crate 和类型名兼容层，
   无法实现唯一入口，因此拒绝。
2. 让 `ax-sync` 直接依赖 `ax-task`。这会形成 `ax-task -> ax-sync -> ax-task` 的层次环，
   也阻止其他 OS 提供 runtime，因此拒绝。
3. 继续允许 crates.io `spin` 只承载 Once/Lazy。它会保留第一方直接依赖和 lint 例外；
   `ax-lazyinit` 可以提供同等 no_std 初始化原语，因此拒绝。
4. 所有路径统一使用 sleep mutex。IRQ、调度器和早期启动路径不能睡眠，因此不成立。
5. 为 rwlock 引入 writer preference。该行为改变超出重构目标，继续保留既有非公平算法。

## 8. 风险、验证与非目标

主要风险是 IRQ/preempt 恢复错误、raw 调用无重入证明、mutex 注册边界丢唤醒、provider
重复链接、Starry facade 绕过以及 Axvisor 把普通状态重新放回内核 spin 锁。
验证覆盖 public spin/rwlock 上下文状态、SMP 可见性、try 失败清理、mutex 多 waiter、
注册边界、非分配 try、owner 诊断、强制解锁、drop、lockdep 顺序和各 OS smoke。

本次明确不做：

- 不改变 Starry 用户态 ABI、syscall 语义或 pthread C 布局；
- 不改变 rwlock 公平性；
- 不为 Axvisor 普通路径引入 `ax-sync`；
- 不把 raw 获取变成安全 API；
- 不手工修改任何 crate 版本号，版本维护由 release-plz 负责。
