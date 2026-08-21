# TGOSKits 锁分层与桥接设计

## 1. 问题、调用方与成功标准

本次修改是高风险、跨 crate 的同步边界重构。修改前，锁算法、IRQ/preempt 上下文、
可睡眠等待和 lockdep 分散在多个公共 crate 中；OS 代码、可移植组件和调度器代码使用
同一类型，却没有清晰表达实现所有权。这样会产生三类问题：

- 调度器既是等待能力的所有者，又通过外部同步 crate 间接使用自己的能力，容易形成
  `ax-task`、`ax-runtime` 和锁 crate 之间的依赖环；
- OS 代码能够绕过自己的 facade，难以审核 IRQ、preempt 和 sleep 语义；
- 测试 provider、生产 provider 和锁算法的职责混在一起，容易重复注册或让测试配置进入
  裸机构建。

调用方分为两类：文件系统、网络、驱动、内存和虚拟化等 OS 无关组件需要稳定的
`ax-sync` API；ArceOS、StarryOS 和 Axvisor 需要使用所属 OS 的原生锁 facade。成功标准是
生产锁算法和 lockdep 状态只有 `ax-task` 一个所有者，OS 无关组件不依赖具体 OS，生产
构建只有 `ax-runtime` 一个 `ax-sync` provider，并保持既有锁语义、Starry 用户态 ABI 和
syscall 行为不变。

## 2. 实现所有权和依赖方向

`ax-task::sync` 是 ArceOS 生产锁实现的唯一所有者：

- `spin/`：`SpinLock`、`SpinRwLock` 及 raw、no-preempt、IRQ-save 获取模式；
- `mutex/`：直接使用 `WaitQueue` 的可睡眠 `Mutex`；
- `lockdep/`：lock class、依赖图、当前任务 held-lock stack、诊断和 trace；
- `context`：preempt 和 IRQ 状态的进入、嵌套及逆序恢复；
- `api`：由 `pub use ...::*` 聚合的稳定原生 API；
- `bridge`：只供 `ax-runtime` 使用的非泛型底层操作。

`ax-sync` 不拥有 ArceOS 生产算法。它按 `interface/`、`spin/`、`mutex/`、`lockdep/`
分层，只保存泛型值、固定原子状态、lock-class 元数据和 RAII guard，并把每次真实操作交给
隐藏接口。稳定出口使用 glob re-export，内部 provider ABI 和 raw 细节不进入普通调用方
API。

生产依赖方向如下：

```text
portable components / drivers / fs / net
                     |
                     v
                  ax-sync
                     ^
                     | provider ABI
ax-task <------ ax-runtime ------> ArceOS / StarryOS facade
   ^                                  ^
   | native API                       |
   +--------- ax-log/display/input ---+

Axvisor ordinary state ----------> std::sync
Axvisor special contexts --------> ax_std::os::arceos::sync
```

具体边界是：

- `ax-task` 不依赖 `ax-sync`；
- `ax-runtime` 始终依赖 `ax-task`，同时依赖 `ax-sync` 以实现桥接 provider；
- ArceOS API、POSIX API、`axstd`、`axlibc` 和 StarryOS 使用 `ax-runtime::sync`；
- Starry kernel 只从 `crate::sync` 导入锁，`crate::sync` 用 glob re-export 聚合 runtime
  facade 和少数专用 wrapper；文件系统拥有的 `FsContext` 保持使用 `ax-fs-ng` 导出的
  `SleepMutex` 类型，并由 `crate::sync::FsMutex` 收口该所有权类型；
- Axvisor 普通路径使用 `std::sync`，IRQ、guest-entry 和 no-preempt 路径使用
  `ax_std::os::arceos::sync`，AxVM 不直接依赖 `ax-sync`；
- `ax-log`、`ax-display`、`ax-input` 属于 ArceOS 内部基础模块，直接使用 `ax-task` 原生锁；
- 为解除经 `ax-task`/`ax-runtime` 产生的真实依赖环，`ax-hal`、`ax-mm`、`ax-ipi` 是
  允许直接依赖 `ax-sync` 的底层例外。新增例外必须以 `cargo metadata` 证明依赖环，并在
  设计材料中记录理由与复核范围。

## 3. 公共锁语义

### 3.1 Spin lock 和执行上下文

锁对象不固化获取上下文，由调用方法表达本次临界区约束：

| 获取方法 | 进入动作 | 退出动作 | 典型场景 |
| --- | --- | --- | --- |
| `lock()` / `try_lock()` | 禁止 preempt | 恢复 preempt 深度 | 不会被本地 IRQ 重入的短临界区 |
| `lock_irqsave()` / `try_lock_irqsave()` | 禁止 preempt，再保存并关闭 IRQ | 恢复 IRQ，再恢复 preempt | IRQ 与任务共享的状态 |
| `unsafe lock_raw()` / `try_lock_raw()` | 不改变上下文 | 不改变上下文 | 外层已建立排他性或已禁止重入 |

`SpinRwLock<T>` 的 read/write 获取提供相同三类模式，并保持既有非公平算法，不引入
writer preference。raw 获取保持 `unsafe`，调用方必须证明同 CPU 不会重入并且并发访问
满足独占或共享规则。所有上下文 guard 和锁 guard 均为 `!Send`，避免把 IRQ/preempt
恢复责任移动到另一线程或 CPU。

独立上下文 guard 的固定顺序为：

```text
acquire: disable_preempt -> irq_save_and_disable
release: irq_restore -> enable_preempt
```

try 获取失败、lockdep 诊断 panic 或原子获取未完成时，pending RAII 状态必须释放已取得的
锁状态并按相反顺序恢复上下文。

### 3.2 可睡眠 Mutex

`Mutex<T>` 始终表示无 poison 的可睡眠 mutex，仅在 `sleep` feature 下提供。ArceOS 原生
实现直接使用 `ax-task::WaitQueue`：

1. 快路径以 Acquire CAS 将 owner 从 0 改为当前 task ID；
2. 首次竞争时分配地址稳定的 `WaitQueue`，以 CAS 安装 opaque 指针，失败者释放候选队列；
3. `WaitQueue` 在调度器所有权下完成“复查 owner、登记 waiter、睡眠”的原子协议；
4. unlock 先以 Release 发布 owner=0，再唤醒至多一个 waiter；
5. drop 只在 owner 为空且没有活动 waiter 时释放队列。

`try_lock` 不调用 `might_sleep`、不分配 wait queue、也不进入调度器。递归获取、错误 owner
解锁和带 waiter 的 drop 都要产生诊断。POSIX pthread 因 C ABI 无法保存 Rust guard，
由 POSIX 专用 wrapper 封装隐藏的 `unsafe force_unlock`，普通调用方不能绕过所有权规则。

## 4. `ax-sync` 桥接边界

`ax-sync::interface` 声明五个隐藏接口：

- `ContextOps`：独立上下文 guard 的 enter/exit；
- `SpinOps`：spin acquire、try、release、状态查询和专用强制释放；
- `RwLockOps`：read/write acquire、try、release 和专用 read decrement；
- `MutexOps`：sleep mutex acquire、try、release、owner 查询和 wait queue 回收；
- `LockdepOps`：不隶属单次获取事务的 trace 开关与 dump。

provider 必须完成一次操作的整个事务，而不是把算法拆回桥接层：进入上下文、lockdep
prepare、原子获取或等待、lockdep commit、失败回滚、Release 解锁和上下文逆序恢复都由
provider 负责。需要恢复执行上下文的 `ax-sync` guard 保存 provider 返回的 opaque restore
token；所有 guard 在 `Drop` 时调用匹配的 release。

桥接接口使用 `extern "Rust"` 的 `ax-crate-interface`，是同一 workspace、同一 pinned Rust
toolchain 下的内部链接契约，不是可跨编译器或跨语言稳定的 C ABI。边界只传递固定布局的
原子引用、裸指针、整数模式、`Location` 和 `#[repr(C)]` 结果；泛型值和 Rust guard 不跨
边界。`LockMetadata` 的 class storage 由 runtime 显式借用并组装为仅在 provider 内部使用的
`ax-task::sync::bridge::LockClass`，后者不跨桥接 ABI；两者必须保持约定的生命周期。生产
provider 只能由 `ax-runtime/src/sync.rs` 实现。

## 5. 条件编译的 host-test 后端

不保留 `ax-sync-test-support` crate。宿主机单元测试启用 `ax-sync/host-test` 时，
`ax-sync` 在最窄模块边界上条件编译一个 std 测试后端：

```text
cfg(all(feature = "host-test", not(target_os = "none")))
```

该后端直接服务 `ax-sync` 和 OS 无关组件的 host tests，不注册
`ax-crate-interface` provider，因此与同时链接的 `ax-runtime` 不会发生 provider 符号冲突。
它用线程局部状态模拟 preempt/IRQ，用 std 条件变量模拟 sleep wait，并执行与生产路径相同
的公开 API 契约测试。

这是测试专用实现，不是第二个生产所有者：`target_os = "none"` 即使误开 `host-test` 也不会
编译 std 后端，仍要求 `ax-runtime` provider；生产 SMP、lockdep 和 host 行为完全由
`ax-task`/runtime 配置决定。`ax-sync` 的公开 feature 只保留 `sleep`、`lock-api`、
`axtest` 和这个测试专用的 `host-test`，不再接受 `smp` 或 `lockdep`。

## 6. Lockdep 所有权

生产 lockdep 的 class graph、held-lock stack、获取模式、trace 和诊断全部属于
`ax-task::sync::lockdep`。spin、rwlock 和 sleep mutex 共享一张依赖图：

- spin 获取标记为禁止 sleep，sleep mutex 标记为允许 sleep；
- 获取前检查递归和反向可达锁序，成功后才提交依赖边和 held-lock；
- 释放校验当前任务 held-lock 与锁实例地址；
- read、write 和 exclusive 使用不同获取模式，同一任务的嵌套 reader 保持现有语义；
- raw read 跨任务切换的特殊路径不绑定普通 task held-lock stack，只由专用 wrapper 使用。

lockdep 被关闭时，trace facade 是 no-op，但不会改变锁算法。`ax-sync/host-test` 中的简化
lockdep 只用于验证递归、锁序和 panic 回滚，不成为生产事实来源。

## 7. 子系统锁使用审计规则

- 能够阻塞、分配或调用调度器的状态使用 sleep `Mutex`，并避免在 guard 内执行未知回调；
- IRQ handler、scheduler-sensitive 和任务/IRQ 共享状态使用 `lock_irqsave()`；
- 普通不可睡眠短临界区使用 `lock()`；只有相邻代码能说明外层排他性时才使用 raw；
- StarryOS 不能绕过 `crate::sync`，迁移只改变内部导入与实现所有权，不改变 syscall 参数、
  返回值、errno、用户结构体、阻塞/唤醒状态机或 pthread C 布局。因此本次不产生需要新增
  Linux syscall 映射表的用户态 ABI 变更；
- Axvisor 的 std 路径不能为了复用内核锁而回退到 `ax-sync`，特殊上下文必须通过 axstd
  facade 表达；
- unlock 前以 Release 发布无 owner 状态，wake/notify 不得在持有宽锁时执行。

迁移完成后不再使用专用的全仓扫描器自动判断这些边界。新增依赖、源码导入、provider
实现或 host 后端配置时，必须按本节约束进行架构评审，并由现有构建与运行矩阵验证实际
组合。

## 8. Prior art 与方案比较

锁语义参考 Linux v6.12：`include/linux/spinlock.h` 的普通、raw 和 irqsave 获取族说明
上下文策略属于获取动作；`kernel/locking/mutex.c` 的 owner 发布、等待和单 waiter 唤醒说明
sleep mutex 不能按 feature 静默退化为 spin；`kernel/locking/lockdep.c` 的 lock class 与
held-lock graph 说明多种锁算法应共享诊断状态。

评估过的方案：

1. 保持算法在 `ax-sync`。这让调度器等待和 lockdep task state 的所有权跨层，并迫使
   `ax-task` 依赖外部实现，拒绝。
2. 让 `ax-sync` 直接依赖 `ax-task`。会破坏 OS 无关边界并形成 runtime/task 依赖环，拒绝。
3. 新增 `ax-sync-test-support` 提供 host provider。它增加一个只为测试存在的 workspace
   crate，还需要保证 provider 被强制链接且不与 runtime 重复注册；改为 `ax-sync` 最窄 cfg
   的内部测试后端。
4. OS 层统一直接依赖 `ax-sync`。这会绕过 runtime/axstd/Starry facade，使锁上下文难以按
   OS 审计，拒绝。
5. 所有状态统一使用 sleep mutex，或给 rwlock 增加 writer preference。前者不适用于 IRQ
   和调度器路径，后者改变既有公平性，均不属于本次重构。

## 9. 风险与验证

主要风险是 IRQ/preempt 恢复错误、try 失败泄漏状态、桥接布局漂移、mutex waiter 注册边界
丢唤醒、lockdep panic 污染后续获取、重复 provider、OS facade 绕过和错误选择 sleep/spin。
验证需要覆盖：

- `ax-task` 原生实现与 `ax-sync` bridge 的 SMP Acquire/Release 可见性和互斥；
- UP、preempt、IRQ 的嵌套与逆序恢复，以及所有 try 失败路径；
- raw 契约、非公平 rwlock、read/write lockdep 模式和锁序诊断；
- mutex 单/多 waiter、unlock/登记边界、逐个唤醒、try 不分配不睡眠、owner、drop 和
  force-unlock wrapper；
- 所有 guard 的 `!Send` 编译失败测试；
- `sync-lint`、目标 crate clippy、rustdoc、ArceOS/Starry QEMU 和 Axvisor
  多架构构建/smoke；
- 最终生产依赖图只有一个 provider，`host-test` 不进入裸机目标。

## 10. 兼容性与非目标

本次明确不做：

- 不改变 Starry 用户态 ABI、syscall、pthread C 布局、阻塞/唤醒语义；
- 不改变 spin rwlock 公平性；
- 不让 Axvisor 普通路径依赖内核锁；
- 不把 raw 获取变成安全 API；
- 不恢复 `ax-kspin`、`ax-kernel-guard`、`ax-lockdep` 或第一方 crates.io `spin`；
- 不修改 `[patch.crates-io]`；
- 不手工修改任何现有 crate 版本号，版本维护由 release-plz 负责。
