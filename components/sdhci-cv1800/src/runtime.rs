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
    /// 阻塞当前任务直至硬件中断唤醒或超时。
    /// 超时返回 `true`，被中断唤醒返回 `false`。
    ///
    /// # 单 waiter 契约
    ///
    /// 调用方保证至多一个任务同时阻塞于此方法（由 SDIO 总线锁序列化）。
    /// OS 胶水层可依赖此保证使用单一共享唤醒队列；丢失唤醒由超时兜底。
    ///
    /// 默认实现使用基于 sleep 的回退，兼容尚未更新为中断驱动唤醒的 OS 胶水层。
    fn block_timeout(&self, timeout_ms: u64) -> bool {
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
