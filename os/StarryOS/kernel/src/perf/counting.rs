//! Owner-CPU extension of finite-width PMU counter values.

/// Per-running-slice state used to extend a raw PMU counter past its width.
///
/// ARM programmable counters are 32-bit. Their sticky overflow bit is consumed
/// either by the PMU IRQ handler or by the owner while stopping the counter;
/// both paths update this same state before a value is published.
#[derive(Debug, Default)]
pub(super) struct CounterExtender {
    completed_wraps: u64,
}

impl CounterExtender {
    pub(super) const fn new() -> Self {
        Self { completed_wraps: 0 }
    }

    pub(super) fn reset(&mut self) {
        self.completed_wraps = 0;
    }

    pub(super) fn record_overflow(&mut self) {
        self.completed_wraps = self.completed_wraps.saturating_add(1);
    }

    pub(super) fn value(&self, raw: u64, width: u16) -> u64 {
        if width >= 64 {
            raw
        } else {
            self.completed_wraps
                .checked_shl(u32::from(width))
                .unwrap_or(u64::MAX)
                .saturating_add(raw & ((1u64 << width) - 1))
        }
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use super::CounterExtender;

    #[test]
    fn extends_a_32_bit_counter_across_overflow() {
        let mut state = CounterExtender::new();
        assert_eq!(state.value(0xffff_fff0, 32), 0xffff_fff0);
        state.record_overflow();
        assert_eq!(state.value(0x10, 32), 0x1_0000_0010);
        state.reset();
        assert_eq!(state.value(0x10, 32), 0x10);
    }
}
