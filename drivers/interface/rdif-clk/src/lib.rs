#![no_std]

extern crate alloc;

use alloc::vec::Vec;

use rdif_base::def_driver;
pub use rdif_base::{DriverGeneric, KError, custom_type};

custom_type!(
    #[doc = "Clock signal id"],
    ClockId, usize, "{:#x}");

/// One immutable MMIO write restriction required while a clock is host-owned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockMmioWriteProtection {
    /// Reject every write that overlaps the byte range.
    Deny { offset: usize, length: usize },
    /// Remove protected value and write-enable bits from an aligned 32-bit write.
    MaskedWrite32 {
        offset: usize,
        value_mask: u32,
        write_enable_mask: u32,
    },
}

pub trait Interface: DriverGeneric {
    fn perper_enable(&mut self);

    fn enable(&mut self, _id: ClockId) -> Result<(), KError> {
        Ok(())
    }

    fn get_rate(&self, id: ClockId) -> Result<u64, KError>;

    fn set_rate(&mut self, id: ClockId, rate: u64) -> Result<(), KError>;

    /// Describes provider-MMIO writes that must be mediated while `id` remains
    /// owned by the host.
    ///
    /// `None` means this provider cannot safely expose its mutable register
    /// window to an assigned guest. An empty vector means the clock has no
    /// mutable provider state that needs mediation.
    fn assignment_mmio_write_protection(
        &self,
        _id: ClockId,
    ) -> Option<Vec<ClockMmioWriteProtection>> {
        None
    }
}

def_driver!(Clk, Interface);
