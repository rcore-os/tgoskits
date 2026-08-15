//! OS 时序能力注入。
//!
//! 控制器驱动需要毫秒级延迟和中断驱动的阻塞等待，
//! 但不能绑定到任何特定内核的任务运行时。
//! OS 胶水层通过 [`set_delay`] 一次性安装 [`SdhciDelay`] 提供者；
//! 驱动通过 [`delay`] 访问它。

use core::sync::atomic::{AtomicPtr, Ordering};

/// SDHCI 控制器所需的 OS 时序能力。
pub trait SdhciDelay: Send + Sync + 'static {
    /// 阻塞延迟指定毫秒数。
    fn delay_ms(&self, ms: u64);
    /// 阻塞当前任务直至 `condition` 满足或超时。
    /// 条件满足返回 `false`，超时返回 `true`。
    ///
    /// # 丢唤醒协议（实现必须满足）
    ///
    /// `condition` 的最终检查与任务入队必须发生在同一关中断临界区内，
    /// 使“检查后、入队前 ISR 已发布完成事件”的情况不可能出现：
    ///
    /// - ISR 在锁内检查前触发：其 latch 的硬件状态位（XFER_COMPLETE sticky）
    ///   由 `condition` 观察到，任务不进入睡眠直接返回；
    /// - ISR 在入队后触发：notify 命中已入队的任务。
    ///
    /// 调用方在调用前负责重新打开中断信号（unmask）；开信号与入队之间的
    /// ISR 不会丢事件——它 latch 的硬件状态位由锁内 `condition` 检查看到。
    ///
    /// 本方法在中断开启的 task 上下文被调用（禁止在 ISR 上下文调用）；
    /// 上文的关中断临界区由实现自行建立（如 `SpinNoIrq` 锁），
    /// 调用方不负责关中断。
    ///
    /// # 单 waiter 契约
    ///
    /// 调用方保证至多一个任务同时阻塞于此方法（由 SDIO 总线锁序列化）。
    /// OS 胶水层可依赖此保证使用单一共享唤醒队列。
    ///
    /// 默认实现退化为“检查一次 + 睡满超时”，事件即时性由调用方重检兜底，
    /// 兼容未实现条件等待的 OS 胶水层。
    fn block_timeout_until(&self, timeout_ms: u64, condition: &dyn Fn() -> bool) -> bool {
        if condition() {
            return false;
        }
        self.delay_ms(timeout_ms);
        true
    }
}

static DELAY: AtomicPtr<&'static dyn SdhciDelay> = AtomicPtr::new(core::ptr::null_mut());

/// 安装时序能力提供者。在初始化期间、操作控制器前调用一次。
/// 不得与 [`delay`] 并发调用。
pub fn set_delay(provider: &'static dyn SdhciDelay) {
    let boxed = alloc::boxed::Box::new(provider);
    let ptr = alloc::boxed::Box::into_raw(boxed);
    let old = DELAY.swap(ptr, Ordering::AcqRel);
    if !old.is_null() {
        unsafe { drop(alloc::boxed::Box::from_raw(old)) };
    }
}

pub(crate) fn delay() -> &'static dyn SdhciDelay {
    let ptr = DELAY.load(Ordering::Acquire);
    assert!(
        !ptr.is_null(),
        "sdhci-cv1800: SdhciDelay not installed; call sdhci_cv1800::set_delay() during init"
    );
    unsafe { *ptr }
}
