//! EL0 access to explicitly authorized PMU counters.

use core::arch::asm;

use super::{Pmu, PmuError, isb, registers};

impl Pmu {
    /// Revokes all direct EL0 PMU access.
    pub fn disable_user_access(&mut self) {
        write_reg!("PMUSERENR_EL0", 0);
        isb();
    }

    /// Grants direct reads of the counters authorized for the current EL0 context.
    ///
    /// # Safety
    /// Every bit in `readable` must belong to the returning user context. On
    /// pre-PMUv3p9 CPUs all other counters must be stopped and unowned: their values are
    /// cleared because hardware cannot restrict EL0 reads per counter. The
    /// caller must revoke access before changing event or user ownership.
    pub unsafe fn enable_user_access(&mut self, readable: u64) -> Result<(), PmuError> {
        if readable & !self.implemented_mask() != 0 {
            return Err(PmuError::InvalidCounter);
        }
        if self.info.version >= 9 {
            write_reg!("S3_0_C9_C14_4", readable); // PMUACR_EL1
        } else {
            for index in 0..self.info.num_counters {
                if readable & (1u64 << index) == 0 {
                    registers::write_counter(index, 0);
                }
            }
            if readable & (1u64 << 31) == 0 {
                write_reg!("PMCCNTR_EL0", 0);
            }
            if self.info.has_instruction_counter && readable & (1u64 << 32) == 0 {
                write_reg!("S3_3_C9_C4_0", 0); // PMICNTR_EL0
            }
        }
        // UEN (PMUv3p9), ER, CR. EN and SW stay clear.
        write_reg!("PMUSERENR_EL0", (1u64 << 4) | (1 << 3) | (1 << 2));
        isb();
        Ok(())
    }
}
