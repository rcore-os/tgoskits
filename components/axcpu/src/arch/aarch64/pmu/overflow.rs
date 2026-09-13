//! Per-counter overflow state and interrupt controls.

use core::arch::asm;

use super::{CounterId, Pmu, PmuError, isb};

impl Pmu {
    /// Returns pending overflow flags for implemented counters.
    pub fn overflow_status(&self) -> u64 {
        read_reg!("PMOVSCLR_EL0") & self.implemented_mask()
    }

    /// Clears only the requested, implemented overflow bits.
    pub fn clear_overflow(&mut self, mask: u64) {
        write_reg!("PMOVSCLR_EL0", mask & self.implemented_mask());
    }

    /// Enables overflow interrupt generation for one counter.
    pub fn enable_overflow_irq(&mut self, id: CounterId) -> Result<(), PmuError> {
        self.validate(id)?;
        write_reg!("PMINTENSET_EL1", 1u64 << id.index());
        Ok(())
    }

    /// Disables an interrupt and clears its pending overflow in Linux order.
    pub fn disable_overflow_irq(&mut self, id: CounterId) -> Result<(), PmuError> {
        self.validate(id)?;
        let mask = 1u64 << id.index();
        write_reg!("PMINTENCLR_EL1", mask);
        isb();
        self.clear_overflow(mask);
        isb();
        Ok(())
    }
}
