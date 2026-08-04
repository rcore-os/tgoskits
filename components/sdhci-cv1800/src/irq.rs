//! SDHCI 中断处理模块
//!
//! 设计模式：
//!   - ISR: 处理 CARD_INT 和 XFER_COMPLETE 中断
//!     - CARD_INT: mask 信号 + 调用回调（通知 WiFi 驱动有数据可读）
//!     - XFER_COMPLETE: mask 信号 + 调用 PIO 唤醒回调（唤醒阻塞的任务）
//!   - PIO: CviSdhci 的 wait_* 方法在 Phase 1 直接轮询 INT_STATUS 寄存器，
//!     Phase 2 通过中断驱动等待（block_timeout）
//!
//! # Single-core assumption
//!
//! SIG_EN 的 RMW（task 侧 unmask 与 ISR 侧 mask）在单 hart 上仍可能被中断
//! 抢占：ISR 可能在 task 的 `mmio_read` 和 `mmio_write` 之间触发。此竞态
//! 由 XFER_COMPLETE sticky bit 自愈——即使 SIG_EN 被错误重写，电平触发的中断线
//! 会在 task 阻塞后重新断言、ISR 重新触发。最坏情况退化为一次 10ms 超时。
//! SMP 平台需额外围栏和 per-hart INT_STATUS 分离。

use core::sync::atomic::{AtomicU16, AtomicU64, AtomicUsize, Ordering};

use crate::{mmio_read, mmio_write, regs::*};

/// Callback slot: stores a function pointer for ISR invocation.
///
/// Uses `AtomicUsize` because `fn()` is pointer-width on all supported
/// targets (riscv64, aarch64, x86_64). A zero value means unregistered;
/// the ISR guards against this.
struct CallbackSlot {
    ptr: AtomicUsize,
}

impl CallbackSlot {
    const fn new() -> Self {
        Self {
            ptr: AtomicUsize::new(0),
        }
    }

    /// Register a callback. Call once during init before IRQs are enabled.
    fn register(&self, cb: fn()) {
        self.ptr.store(cb as usize, Ordering::Release);
    }

    /// Invoke the registered callback, if any.
    ///
    /// # Safety
    ///
    /// - The stored value must be a valid `fn()` previously stored via
    ///   `register`. A zero value (empty slot) is guarded and does not
    ///   invoke the callback.
    /// - Single-core assumption: no concurrent store to this slot from another
    ///   hart while the callback is being invoked.
    /// - The callback runs in hard-IRQ context on the calling hart and must not
    ///   allocate, hold locks, schedule, or call `log` macros that synchronously
    ///   write UART.
    unsafe fn invoke(&self) {
        let v = self.ptr.load(Ordering::Acquire);
        if v != 0 {
            // SAFETY: v was stored as `cb as usize` by register().
            // fn() and usize are the same size on all supported targets.
            let cb: fn() = unsafe { core::mem::transmute::<usize, fn()>(v) };
            cb();
        }
    }
}

/// SDHCI 中断全局状态
struct SdhciIrqState {
    /// SDHCI MMIO 基地址（ISR 裸写用）
    base: AtomicUsize,
    /// CARD_INT 回调（通知上层驱动有数据可读）
    card_irq_callback: CallbackSlot,
    /// XFER_COMPLETE PIO 唤醒回调（唤醒阻塞在 block_timeout 的任务）
    pio_wake_callback: CallbackSlot,
}

impl SdhciIrqState {
    const fn new() -> Self {
        Self {
            base: AtomicUsize::new(0),
            card_irq_callback: CallbackSlot::new(),
            pio_wake_callback: CallbackSlot::new(),
        }
    }
}

static SDHCI_IRQ_STATE: SdhciIrqState = SdhciIrqState::new();

pub static SDHCI_IRQ_COUNT: AtomicU64 = AtomicU64::new(0);
pub static SDHCI_LAST_NORM: AtomicU16 = AtomicU16::new(0);
pub static SDHCI_CARD_INT_COUNT: AtomicU64 = AtomicU64::new(0);

/// 初始化 ISR 全局状态（设置 MMIO 基地址）
pub fn irq_state_init(base: usize) {
    SDHCI_IRQ_STATE.base.store(base, Ordering::Release);
}

/// 注册 CARD_INT 回调函数
///
/// WiFi 驱动初始化时调用，注册一个函数用于在 ISR 中通知"卡有数据可读"。
/// 回调在硬中断上下文执行，禁止：持锁、分配堆、调度、调用 log。
pub fn register_card_irq_callback(cb: fn()) {
    SDHCI_IRQ_STATE.card_irq_callback.register(cb);
}

/// 注册 PIO 唤醒回调函数
///
/// OS 胶水层在初始化时调用，注册一个函数用于在 ISR 中唤醒阻塞在
/// `block_timeout` 上的任务。回调在硬中断上下文执行，禁止：持锁、分配堆、
/// 调度、调用 log。
pub fn register_pio_wake_callback(cb: fn()) {
    SDHCI_IRQ_STATE.pio_wake_callback.register(cb);
}

/// 使能 CARD_INT 中断信号（XFER_COMPLETE 由 poll_int_status 动态 un-mask）
pub fn enable_irq_signals() {
    let base = SDHCI_IRQ_STATE.base.load(Ordering::Acquire);
    if base == 0 {
        return;
    }
    mmio_write::<u16>(base + SDHCI_NORM_INT_SIG_EN as usize, NORM_INT_SIG_MASK);
    mmio_write::<u16>(base + SDHCI_ERR_INT_SIG_EN as usize, ERR_INT_SIG_MASK);
}

/// 禁用所有 SDHCI 中断信号
pub fn disable_irq_signals() {
    let base = SDHCI_IRQ_STATE.base.load(Ordering::Acquire);
    if base == 0 {
        return;
    }
    mmio_write::<u16>(base + SDHCI_NORM_INT_SIG_EN as usize, 0);
    mmio_write::<u16>(base + SDHCI_ERR_INT_SIG_EN as usize, 0);
}

// ── SIG_EN 寄存器 RMW 辅助函数 ──
//
// 所有 SIG_EN 的 read-modify-write 操作集中在此，
// 方便审查单核竞态假设和未来 SMP 适配。
// 注意：enable_irq_signals/disable_irq_signals 是全量写入（reset 语义），不属于 RMW。

/// Read-Modify-Write on `SDHCI_NORM_INT_SIG_EN`.
///
/// RMW 本身不是原子的（可能被 ISR 抢占），但设计依赖 XFER_COMPLETE sticky bit
/// 自愈：即使本次写入携带过期值，中断线在 task 阻塞后重新断言，ISR 重新触发。
fn rmw_norm_sig_en(base: usize, set: u16, clear: u16) {
    let addr = base + SDHCI_NORM_INT_SIG_EN as usize;
    let cur = mmio_read::<u16>(addr);
    mmio_write::<u16>(addr, (cur & !clear) | set);
}

/// 启用 XFER_COMPLETE 中断信号（在 poll_int_status 阻塞前调用）
///
/// 注意：SDHCI ISR 收到 XFER_COMPLETE 后会立即 mask 掉该信号，
/// 因此每次阻塞前都需要重新调用此函数。
pub fn unmask_xfer_complete_signal() {
    let base = SDHCI_IRQ_STATE.base.load(Ordering::Acquire);
    if base == 0 {
        return;
    }
    rmw_norm_sig_en(base, NORM_INT_XFER_COMPLETE, 0);
}

/// 屏蔽/恢复 CARD_INT 信号（裸地址操作，ISR 安全）
pub(crate) fn mask_card_irq_raw(base: usize, mask: bool) {
    if mask {
        rmw_norm_sig_en(base, 0, NORM_INT_CARD_INT);
    } else {
        rmw_norm_sig_en(base, NORM_INT_CARD_INT, 0);
    }
}

/// SDHCI 中断处理函数（注册到 PLIC）
///
/// 处理两种中断：
/// - CARD_INT: mask 信号 + 调用回调（通知 WiFi 驱动有数据可读）
/// - XFER_COMPLETE: mask 信号 + 调用 PIO 唤醒回调（不消费锁存位，由任务端 W1C）
///   PIO 事件（CMD_COMPLETE / BUF_RD_READY / BUF_WR_READY）由 wait 函数 Phase 1 直接轮询。
pub fn sdhci_irq_handler(_irq: usize) {
    SDHCI_IRQ_COUNT.fetch_add(1, Ordering::Relaxed);

    let base = SDHCI_IRQ_STATE.base.load(Ordering::Acquire);
    if base == 0 {
        return;
    }

    let status = mmio_read::<u32>(base + SDHCI_INT_STATUS_NORM as usize);
    if status == 0 {
        return;
    }

    let norm = status as u16;
    SDHCI_LAST_NORM.store(norm, Ordering::Relaxed);

    if norm & NORM_INT_CARD_INT != 0 {
        SDHCI_CARD_INT_COUNT.fetch_add(1, Ordering::Relaxed);
        mask_card_irq_raw(base, true);
        // SAFETY: CARD_INT callback is registered once during init via
        // register_card_irq_callback. Single-core: no concurrent store.
        unsafe { SDHCI_IRQ_STATE.card_irq_callback.invoke() };
    }

    // Handle XFER_COMPLETE: mask signal, then wake blocked task.
    // The sticky status bit is NOT cleared here — the task observes and
    // W1C-clears it after wake. Clearing it here would destroy the
    // wake condition and cause guaranteed 200ms timeout.
    // XFER_COMPLETE is normally only enabled in SIG_EN while a task is
    // blocked in poll_int_status (see unmask_xfer_complete_signal), so
    // this path is idle in steady state. After a race-guard win or timeout
    // the SIG_EN bit may briefly remain set; the resulting spurious ISR
    // re-masks and notifies harmlessly.
    if norm & NORM_INT_XFER_COMPLETE != 0 {
        // Mask the XFER_COMPLETE signal to prevent re-firing
        rmw_norm_sig_en(base, 0, NORM_INT_XFER_COMPLETE);
        // Wake the blocked task (status bit stays sticky for task to observe)
        // SAFETY: PIO wake callback is registered once during init via
        // register_pio_wake_callback. Single-core: no concurrent store.
        unsafe { SDHCI_IRQ_STATE.pio_wake_callback.invoke() };
    }
}
