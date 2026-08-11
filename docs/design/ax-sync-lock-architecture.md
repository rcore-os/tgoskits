# TGOSKits 锁实现、桥接与 OS facade 分层

## 1. 问题与目标

本设计是高风险、跨 crate 的同步架构重构。锁算法、执行上下文、调度等待、PI donation
和 lockdep 如果同时散落在 `ax-sync`、`ax-task` 与 runtime provider 中，会出现两类根本
问题：同一个状态有多个事实源，或者调度器通过外部锁 crate 间接调用自己拥有的能力。
这两种结构都会放大丢唤醒、错误 handoff、锁顺序和 feature 组合问题。

目标调用方分为两类：

- 驱动、文件系统、网络、内存和其他 OS 无关组件需要稳定的 `ax-sync` API；
- ArceOS、StarryOS、`ax-std` 和 Axvisor 需要使用所属 OS 的原生锁 facade。

成功标准是：

1. 生产锁算法、执行上下文事务、PI 状态机和 lockdep 只有 `ax-task::sync` 一个实现所有者；
2. `ax-sync` 不依赖 `ax-task`、`ax-hal`、`ax-runtime` 或其他 OS 组件；
3. OS 无关组件仍可使用固定布局、稳定 API 的 `ax-sync` 锁；
4. `ax-runtime` 是 ArceOS 唯一的 `ax-sync` 生产 provider，同时重导 `ax-task` 原生锁；
5. 本分支已有的 Linux-RT 风格 PI waiter、generation、donation、park/wake 和 handoff
   语义不因分层迁移而退化；
6. Starry 用户态 ABI、syscall 行为、pthread C 布局和 spin rwlock 公平性不变。

## 2. 依赖方向

终态依赖图如下：

```text
portable components / drivers / fs / net
                     |
                     v
                  ax-sync
                     ^
                     | hidden provider ABI
                     |
ax-task <-------- ax-runtime --------> ArceOS / StarryOS facade
   ^                 |
   | native API      +-------------> ax-std / axlibc
   +------ ax-log / ax-display / ax-input

Axvisor ordinary state ----------> std::sync
Axvisor special contexts --------> ax_std::os::arceos::sync
```

硬约束如下：

- `ax-task` 不依赖 `ax-sync`。锁的 native API、算法和 task-owned 状态都在
  `ax-task::sync`。
- `ax-sync` 只依赖基础 crate；所有 OS 能力通过隐藏 provider ABI 请求。
- `ax-runtime` 同时依赖 `ax-task` 与 `ax-sync`：它从 `ax-task::sync::api` 重导原生锁，
  并用 `ax-task::sync::bridge` 实现 `ax-sync` provider。
- Starry kernel 只从 `crate::sync` 导入锁；`crate::sync` 只聚合
  `ax-runtime::sync` 与少量语义明确的 wrapper。
- ArceOS API、POSIX API、`ax-std` 和 `axlibc` 使用 `ax-runtime::sync`。
- Axvisor 普通任务状态使用真实 `std::sync`；IRQ、guest-entry 和 no-preempt 路径使用
  `ax_std::os::arceos::sync`。
- `ax-log`、`ax-display`、`ax-input` 属于 ArceOS 内部基础模块，可直接使用
  `ax-task::sync` native API。
- 为解除真实依赖环，`ax-hal`、`ax-mm`、`ax-ipi` 等比 `ax-task` 更低的模块可使用
  `ax-sync`。新增例外必须以 `cargo metadata` 证明依赖环，并加入 `lock-lint` 显式规则。
- 不允许 `ax-kspin`、`ax-kernel-guard`、`ax-lockdep` 或第一方 crates.io `spin`
  重新进入依赖图。

## 3. `ax-task::sync` 的实现所有权

`ax-task::sync` 按职责分层：

- `api`：稳定的 native 锁与 guard 出口；
- `context`：preempt、IRQ 与组合 guard 的进入、嵌套和逆序恢复；
- `spin`：native `SpinLock`、`SpinRwLock`、raw/no-preempt/IRQ-save 获取；
- `mutex`：本分支的 urgency-ordered PI mutex；
- `lockdep`：lock class、依赖图、task held-lock stack、trace 与诊断；
- `bridge`：只供 `ax-runtime` 调用的非泛型 external-layout 事务。

`bridge` 不是第二套实现。native 锁和 external `ax-sync` wrapper 必须调用同一组内部算法。
native 路径直接使用本 crate 拥有的布局；bridge 路径把 `ax-sync` 的固定原子字段借用为
external layout view。禁止 bridge 复制 owner、waiter、grant、donation 或 wake 状态。

`ax-task` 不直接访问 `ax-hal`。IRQ、preempt、current-thread、scheduler entry 和硬中断
状态继续通过 `TaskRuntime` 能力边界取得。这样 native 锁属于调度层，但具体 OS/架构实现
仍由 runtime 提供，Cargo 依赖方向不反转。

## 4. `ax-sync` 薄桥接层

`ax-sync` 只拥有稳定 wrapper 所必需的表示与 Rust API：

- 泛型数据 `T`；
- 固定布局的原子锁状态与 lock metadata；
- `Deref`/`DerefMut`、RAII guard 和 guard 的 `Drop`；
- provider 返回的 opaque context restore token；
- 与 pinned toolchain 一起演进的隐藏内部 ABI。

它不拥有：

- spin、rwlock 或 PI mutex 算法；
- task identity、task registry、runqueue 或调度策略；
- waiter 节点、donation graph、effective priority 或 `blocked_on`；
- IRQ/preempt 的硬件实现；
- 生产 lockdep 图或 task held-lock stack；
- fallback、超时重试或另一套生产 provider。

隐藏接口按完整事务划分：

- `ContextOps`：独立 context guard enter/exit；
- `SpinOps`：spin acquire/try/release 与受控 force-release；
- `RwLockOps`：read/write acquire/try/release 与受控 read decrement；
- `PiMutexOps`：PI acquire/try/release/cancel/force-release、owner 查询和 external storage
  销毁；
- `LockdepOps`：不隶属单次获取的 trace 控制与 dump。

provider 必须完成一次操作的整个事务：进入 context、lockdep prepare、原子获取或 waiter
注册、donation/park/handoff、lockdep commit、失败回滚、Release 解锁和 context 逆序恢复。
不能把这些步骤拆回 `ax-sync`，也不能让 runtime 在两次 provider 调用之间保存中间事实。

ABI 只传固定布局的原子引用、裸指针、整数模式、`Location` 和 `#[repr(C)]` 结果；泛型
值、Rust guard、task handle 和 scheduler 对象不跨边界。所有 raw pointer 都必须在相邻
代码记录生命周期、唯一性、对齐和别名不变量。

## 5. 公共锁语义

### 5.1 Spin lock 与执行上下文

锁对象不固化获取上下文，调用方法表达本次约束：

| 获取方法 | 进入动作 | 退出动作 | 典型场景 |
| --- | --- | --- | --- |
| `lock()` / `try_lock()` | 禁止 preempt | 恢复 preempt token | 不会被本地 IRQ 重入的短临界区 |
| `lock_irqsave()` / `try_lock_irqsave()` | 禁止 preempt，再保存并关闭 IRQ | 恢复 IRQ，再恢复 preempt | IRQ 与任务共享状态 |
| `unsafe lock_raw()` / `try_lock_raw()` | 不改变 context | 不改变 context | 外层已建立排他性 |

`SpinRwLock<T>` 提供相同三种策略并保留非公平算法，不引入 writer preference。raw 获取
保持 `unsafe`，调用方必须证明 UP 与 SMP 下都不会发生同 CPU 重入或违反共享/独占规则。

所有 context guard 和锁 guard 都是 `!Send`。组合顺序固定为：

```text
acquire: disable_preempt -> irq_save_and_disable
release: irq_restore -> enable_preempt
```

try 失败、lockdep 诊断 panic 或部分获取 unwind 时，pending RAII 状态必须释放已取得的
锁状态，并按相反顺序恢复 context。

### 5.2 PI mutex

`Mutex<T>` 与 `PiMutex<T>` 始终表示无 poison 的可睡眠 PI mutex；它们不会因 feature
退化为 spin lock。当前 PI 语义必须完整迁入 `ax-task::sync::mutex`：

1. lock-local state 包含 owner word、waiter bit、generation 和固定大小的 opaque waiter
   storage；
2. task-local state 包含 waiter node、`blocked_on`、donor tree、effective priority、grant
   与 park handshake；
3. waiter 注册在同一事务内提交 owner snapshot、ordered waiter tree 和 donation edge；
4. ownerless handoff、deboost、选择、wake 和 claim 由 task/scheduler owner 协调；
5. cancel 与 timeout 必须通过 generation 验证，不能把旧 wake 当成新一代 waiter；
6. `try_lock` 不调用 `might_sleep`，不初始化 waiter storage，不分配，也不进入 scheduler；
7. release、claim、cancel、waiter 注册和 drop 不分配；
8. drop 只在 waiter tree 为空且没有可达 lock reference 时销毁 external storage。

native mutex 与 `ax-sync` wrapper 的字段可以位于不同对象，但算法、状态转换和顺序必须
共享同一实现。`ax-sync` 只能承载 external 固定布局，不能重新实现 PI fast path 或 claim。

POSIX pthread 因 C ABI 无法保存 Rust guard，只能经专用 wrapper 泄漏 guard，并调用隐藏
的 `unsafe force_unlock`；该路径仍须验证当前 task 是 owner。

## 6. Lockdep 所有权

生产 lockdep 完全属于 `ax-task::sync::lockdep`：lock class、依赖图、task held-lock stack、
获取模式、trace 和诊断只有一个事实源。`ax-sync` 只保存 external wrapper 的固定 class
metadata，provider 将其借用为 ax-task 的 external lock class view。

spin、rwlock 和 PI mutex 共享同一张图：

- spin 获取标记 `sleep_forbidden=true`，PI mutex 标记 `false`；
- 获取前检查递归和反向可达路径，成功后再提交依赖边；
- release 校验 task held-lock 栈顶与实例地址；
- read、write、exclusive 和 subclass 是明确的 typed mode；
- raw read 跨 task 的特殊路径只能由专用 wrapper 使用。

lockdep 被关闭时 trace API 是 no-op，但锁算法和执行上下文不改变。

## 7. Host test 与 provider 选择

`ax-sync/host-test` 仅在以下最窄边界编译内部 std backend：

```text
cfg(all(feature = "host-test", not(target_os = "none")))
```

该 backend 服务 OS 无关组件的 host test，不注册生产 provider，也不成为第二个算法事实
源。`target_os = "none"` 即使误开 `host-test` 仍要求 runtime provider。

`ax-task` host test 使用测试 `TaskRuntime` provider 验证 native 算法；不得通过依赖
feature 反向选择 `ax-sync` host backend。最终 runtime 决定生产或 host 组合，底层 crate
不能自行猜测 provider。

## 8. 子系统规则与机器约束

- OS 无关组件使用 `ax-sync`；可以阻塞的状态使用 PI mutex，IRQ/短临界区使用明确的
  spin 获取策略。
- StarryOS 只使用 `crate::sync`，不能直接依赖 `ax-sync` 或穿透 `ax-task::sync::bridge`。
- `ax-std`、ArceOS API 和 POSIX API 只使用 `ax-runtime::sync`。
- Axvisor 普通路径使用 `std::sync`，特殊路径使用 `ax_std::os::arceos::sync`。
- OS facade 不能导出 `bridge`；external-layout API 保持 `#[doc(hidden)]`。
- `lock-lint` 检查旧锁 crate、直接 crates.io `spin`、OS facade 绕过、provider 唯一性、
  `ax-task -> ax-sync` 反向依赖、host cfg 和底层依赖例外。
- `sync-lint` 检查 sleep mutex 不得在 spin/IRQ/preempt-disabled 临界区内获取。

不能用 allowlist 掩盖未完成的普通迁移。依赖环例外必须是路径级、原因明确、可由依赖图
复核的最小集合。

## 9. Linux PREEMPT_RT 对照

参考源码为本地 Linux `v7.1`，commit
`8cd9520d35a6c38db6567e97dd93b1f11f185dc6`：

- `include/linux/rtmutex.h` 的 `rt_mutex_base` 保存 lock-local owner/waiters，而 task PI
  状态不放进通用 wrapper；
- `kernel/locking/rtmutex.c` 由 `lock->wait_lock` 保护 lock waiter tree，由
  `task->pi_lock` 保护 `pi_waiters`/`pi_blocked_on`，chain walk 和 priority 更新属于 task；
- unlock 在 wake 前完成 top waiter、deboost 与 handoff 协调，避免双事实源和
  deboost-after-wake；
- `include/linux/spinlock.h` 的普通/raw/irqsave 获取族说明 context 策略属于获取动作；
- `kernel/locking/lockdep.c` 的 class/held-lock graph 说明不同锁算法必须共享诊断状态。

本设计借鉴的是所有权与事务顺序，不复制 Linux 对象布局。TGOSKits 的 generation-bearing
task identity、runtime capability 和固定 external ABI 仍由本项目边界表达。

## 10. 方案比较

1. **保持 `ax-sync` 为算法所有者。** 会让调度器通过外部 crate 使用自己的 PI/task
   能力，并使 lockdep/task state 跨层，拒绝。
2. **让 `ax-sync` 依赖 `ax-task`。** 会破坏 OS 无关边界并形成依赖环，拒绝。
3. **采用 #1962 的普通 WaitQueue mutex。** 分层正确，但会丢失本分支 urgency ordering、
   generation waiter、donation chain 和 ownerless handoff，拒绝该具体实现。
4. **采用 #1962 分层并迁移本分支 PI 实现。** 依赖方向正确，同时保留 Linux-RT 风格
   语义，选用。
5. **所有状态统一为 sleep mutex。** IRQ、调度器和早期启动路径不能睡眠，不成立。
6. **保留第二套 host/provider crate。** 增加重复注册和 feature 漂移风险，拒绝。

## 11. 迁移与验证

迁移按可审计层次进行：

1. 建立 `ax-task::sync::{api,bridge}` 命名空间，runtime 只经 bridge 使用 task 能力；
2. 迁移 context 与 native spin/rwlock，不改变 external `ax-sync` API；
3. 迁移 lockdep 图和 task held-lock state，建立 external class view；
4. 将当前 PI physical/task transaction 移入 ax-task，并建立 external PI layout view；
5. 把 `ax-sync` 改为 wrapper/provider ABI，删除 `ax-task -> ax-sync`；
6. 收紧 lint 与 Cargo feature，迁移 OS consumers，最后清理兼容路径。

每一步必须有在旧实现上失败、在新实现上通过的确定性测试。最终验证至少包括：

- ax-task native 与 ax-sync bridge 的 UP/SMP Acquire/Release、try/unwind 和 `!Send`；
- PI 单/多 waiter、priority ordering、donation chain、ownerless handoff、cancel/timeout、
  generation、防丢唤醒、非分配 fast/slow path 与 drop；
- lockdep 递归、反向锁序、subclass、sleep-in-atomic 和 panic rollback；
- `cargo fmt`、目标 crate clippy、rustdoc、`lock-lint`、`sync-lint`；
- StarryOS 四架构 QEMU 与 Axvisor 四架构 build/smoke；
- 与 `dev` 相同命令的耗时对比，明显变慢视为实现缺陷，不能用 timeout 或轮询兜底。

## 12. 非目标

- 不改变 Starry syscall、用户 ABI、pthread C 布局或 errno；
- 不改变 spin rwlock 公平性；
- 不把 raw 获取变成安全 API；
- 不为 Axvisor 普通路径引入内核锁；
- 不使用 timeout、fallback、全局 task registry 或第二份 waiter 状态掩盖模型问题；
- 不修改 `[patch.crates-io]`；
- 不手工修改 crate 版本号，版本由 release-plz 维护。
