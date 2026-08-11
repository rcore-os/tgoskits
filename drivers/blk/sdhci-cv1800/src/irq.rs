//! SDHCI 中断处理模块
//!
//! 设计模式：
//!   - ISR: 裸函数，只处理 CARD_INT (mask 信号 + 调用回调)
//!   - PIO: CviSdhci 的 wait_* 方法直接轮询 INT_STATUS 寄存器
//!   - 分离 ISR 和 PIO 事件避免竞态条件

use core::sync::atomic::{AtomicU16, AtomicU64, AtomicUsize, Ordering};

use crate::{mmio_read, mmio_write, regs::*};

/// SDHCI 中断全局状态
struct SdhciIrqState {
    /// SDHCI MMIO 基地址（ISR 裸写用）
    base: AtomicUsize,
    /// CARD_INT 回调（通知上层驱动有数据可读）
    card_irq_callback: AtomicUsize, // fn() 的裸指针
}

impl SdhciIrqState {
    const fn new() -> Self {
        Self {
            base: AtomicUsize::new(0),
            card_irq_callback: AtomicUsize::new(0),
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
    SDHCI_IRQ_STATE
        .card_irq_callback
        .store(cb as usize, Ordering::Release);
}

/// 使能 CARD_INT 中断信号（ISR 仅处理 CARD_INT，PIO 事件由轮询处理）
pub fn enable_irq_signals() {
    let base = SDHCI_IRQ_STATE.base.load(Ordering::Acquire);
    mmio_write::<u16>(base + SDHCI_NORM_INT_SIG_EN as usize, NORM_INT_SIG_MASK);
    mmio_write::<u16>(base + SDHCI_ERR_INT_SIG_EN as usize, ERR_INT_SIG_MASK);
}

/// 禁用所有 SDHCI 中断信号
pub fn disable_irq_signals() {
    let base = SDHCI_IRQ_STATE.base.load(Ordering::Acquire);
    mmio_write::<u16>(base + SDHCI_NORM_INT_SIG_EN as usize, 0);
    mmio_write::<u16>(base + SDHCI_ERR_INT_SIG_EN as usize, 0);
}

/// 屏蔽/恢复 CARD_INT 信号（裸地址操作，ISR 安全）
pub(crate) fn mask_card_irq_raw(base: usize, mask: bool) {
    let addr = base + SDHCI_NORM_INT_SIG_EN as usize;
    let cur = mmio_read::<u16>(addr);
    mmio_write::<u16>(
        addr,
        (cur & !NORM_INT_CARD_INT) | (!mask as u16 * NORM_INT_CARD_INT),
    );
}

/// SDHCI 中断处理函数（注册到 PLIC）
///
/// 只处理 CARD_INT：mask 信号 + 调用回调。
/// PIO 事件（CMD_COMPLETE / BUF_RD_READY / XFER_COMPLETE）由 wait 函数直接轮询。
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
        let cb = SDHCI_IRQ_STATE.card_irq_callback.load(Ordering::Acquire);
        if cb != 0 {
            unsafe { core::mem::transmute::<usize, fn()>(cb)() };
        }
    }
}
