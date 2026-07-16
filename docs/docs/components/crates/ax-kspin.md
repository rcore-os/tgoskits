# `ax-kspin`

> 路径：`components/kspin`
> 类型：`no_std` 库 crate
> 分层：组件层 / OS 无关自旋锁适配层

`ax-kspin` 将 `spin 0.12` 的原子锁算法、`lock_api 0.4` 的安全数据 guard
和运行时提供的 IRQ/抢占能力组合起来。它不依赖具体 OS、调度器、per-CPU
实现或 HAL。

## 架构

- `RawSpinLock<C>`：FIFO ticket mutex，所有构建始终保留 SMP 原子互斥。
- `RawSpinRwLock<C>`：自旋读写锁；不承诺有界 writer 等待，实时路径优先使用 mutex。
- `SpinMutex` / `SpinRwLockCore`：基于 `lock_api` 的安全数据包装。
- `LockRuntime`：显式 Rust ABI trait-ffi，由 OS runtime 提供 IRQ、抢占、调度安全点和无分配 lockdep hook。
- `IrqGuard`、`PreemptGuard`、`PreemptIrqGuard`：独立上下文 guard。
- `PreemptOnce<T>` / `PreemptLazy<T>`：task-context 单次初始化；initializer 从竞争
  `Once` ownership 前到发布完成始终持有 `PreemptGuard`，但不关闭本地 IRQ。

常用锁类型包括：

- `SpinRaw<T>`：只提供原子互斥，不改变执行上下文。
- `SpinNoPreempt<T>`：持锁期间关闭抢占。
- `SpinIrqSave<T>`：持锁期间关闭本地 IRQ。
- `SpinNoPreemptIrqSave<T>` / `SpinNoIrq<T>`：同时关闭抢占和本地 IRQ。

所有 `lock_api` guard 都使用 `GuardNoSend`，必须在获取它的 CPU 上释放。解锁
顺序固定为：释放 raw lock、退出 IRQ 上下文、退出抢占上下文。运行时以 per-CPU
nesting 保存最外层 IRQ flags，因此多个 guard 即使不按获取顺序 drop，也不会提前
打开 IRQ。

## 运行时边界

最终 OS 必须实现 `LockRuntime`：

```rust,ignore
use ax_kspin::{LockRuntime, impl_trait};

struct Runtime;

impl_trait! {
    impl LockRuntime for Runtime {
        // Forward IRQ hooks to raw arch/HAL primitives and preemption hooks
        // to the task system. See the trait documentation for the full ABI.
    }
}
```

测试二进制也必须提供自己的 fake runtime。crate 不提供默认 host 实现，避免默认
符号与真实 OS runtime 冲突。

## 关键约束

1. `try_lock` 失败必须回滚已经进入的 IRQ/抢占上下文。
2. raw lock 必须先于上下文 guard 退出，避免在数据仍被访问时触发调度。
3. `MutexGuard::unlocked` 会临时完整恢复 IRQ/抢占状态，再重新进入并加锁。
4. `force_unlock` 仅能释放当前 CPU 拥有且被明确 forget 的 guard。
5. hard IRQ lockdep hook 必须写入预分配的 per-CPU 缓冲，不得分配、阻塞或调用任意 observer。
6. scheduler online 后，可能被多个 task 竞争的 lazy initializer 不得直接使用
   `spin::Once` / `spin::LazyLock`。raw Once owner 在发布 `Running` 后被同 CPU
   抢占，而 replacement task 在 IRQ/preempt-disabled 区域等待同一 Once，会形成
   owner 永远无法恢复的抢占反转。普通 task/deferred 路径使用 `PreemptOnce`；early
   boot、CPU offline 或 hard-IRQ 专用对象必须另行证明不存在竞争、抢占与分配。

## 验证

`components/kspin` 测试覆盖无 feature 的多核互斥、非 LIFO IRQ guard、try-lock
回滚、解锁/恢复/调度顺序、临时 unlocked、preemption-aware Once initializer 以及
Mutex/RwLock 公共 API。
