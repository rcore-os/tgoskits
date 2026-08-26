//! Move-only SDHCI controller interrupt endpoint.
//!
//! PIO command/data events are polled by [`crate::CviSdhci`]. The hard endpoint
//! handles only `CARD_INT`: it reads bounded status, masks that signal and
//! reports whether the shared physical IRQ belongs to this controller. No
//! global MMIO base or callback is retained.

use sdio_host::{SdioIrqSource, SdioIrqStatus};

use crate::{mmio_read, mmio_write, regs::*};

/// Move-only hard-IRQ source extracted from one CV1800 SDHCI controller.
pub(crate) struct CviSdhciIrqSource {
    base: usize,
}

impl CviSdhciIrqSource {
    pub(crate) const fn new(base: usize) -> Self {
        Self { base }
    }
}

impl SdioIrqSource for CviSdhciIrqSource {
    fn handle_irq(&mut self) -> SdioIrqStatus {
        let norm = mmio_read::<u16>(self.base + SDHCI_INT_STATUS_NORM as usize);
        if !card_irq_asserted(norm) {
            return SdioIrqStatus::Spurious;
        }

        // CARD_INT is a card-driven level. Draining the card FIFO clears the
        // source; writing the controller W1C bit here would lose that ownership
        // information. Mask before publishing task-context work instead.
        mask_card_irq_raw(self.base, true);
        SdioIrqStatus::CardPending
    }
}

const fn card_irq_asserted(normal_status: u16) -> bool {
    normal_status & NORM_INT_CARD_INT != 0
}

/// Enables only the controller signals consumed by the network IRQ endpoint.
pub(crate) fn enable_irq_signals(base: usize) {
    mmio_write::<u16>(base + SDHCI_NORM_INT_SIG_EN as usize, NORM_INT_SIG_MASK);
    mmio_write::<u16>(base + SDHCI_ERR_INT_SIG_EN as usize, ERR_INT_SIG_MASK);
}

/// Disables every controller interrupt signal without changing status-enable
/// bits used by the polling PIO path.
pub(crate) fn disable_irq_signals(base: usize) {
    mmio_write::<u16>(base + SDHCI_NORM_INT_SIG_EN as usize, 0);
    mmio_write::<u16>(base + SDHCI_ERR_INT_SIG_EN as usize, 0);
}

/// Masks or unmasks only CARD_INT while preserving unrelated signal bits.
pub(crate) fn mask_card_irq_raw(base: usize, mask: bool) {
    let addr = base + SDHCI_NORM_INT_SIG_EN as usize;
    let current = mmio_read::<u16>(addr);
    let next = if mask {
        current & !NORM_INT_CARD_INT
    } else {
        current | NORM_INT_CARD_INT
    };
    mmio_write::<u16>(addr, next);
}

/// Unmasks CARD_INT and closes the rearm window with a status readback.
///
/// CARD_INT is a card-driven level. If it is already asserted when the signal
/// is enabled, keep it masked and let the fixed-CPU poll owner drain it again.
pub(crate) fn rearm_card_irq_and_check(base: usize) -> bool {
    mask_card_irq_raw(base, false);
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    let pending = card_irq_asserted(mmio_read::<u16>(base + SDHCI_INT_STATUS_NORM as usize));
    if pending {
        mask_card_irq_raw(base, true);
    }
    pending
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_irq_is_spurious_without_card_int() {
        assert!(!card_irq_asserted(0));
        assert!(!card_irq_asserted(NORM_INT_CMD_COMPLETE));
        assert!(!card_irq_asserted(NORM_INT_XFER_COMPLETE));
        assert!(card_irq_asserted(NORM_INT_CARD_INT));
        assert!(card_irq_asserted(NORM_INT_CARD_INT | NORM_INT_CMD_COMPLETE));
    }
}
