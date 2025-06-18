use super::GeneralRegisters;
use alloc::format;
use core::fmt::Debug;

/// The comparison result of all general-purpose registers after a change.
pub struct GeneralRegistersDiff {
    old: GeneralRegisters,
    new: GeneralRegisters,
}

impl GeneralRegistersDiff {
    const INDEX_RANGE: core::ops::Range<u8> = 0..16;
    const RSP_INDEX: u8 = 4;

    /// Creates a new `GeneralRegistersDiff` instance by comparing two `GeneralRegisters` instances.
    pub fn new(old: GeneralRegisters, new: GeneralRegisters) -> Self {
        GeneralRegistersDiff { old, new }
    }

    /// Returns `true` if all general-purpose registers are unchanged.
    pub fn is_same(&self) -> bool {
        self.old == self.new
    }
}

impl Debug for GeneralRegistersDiff {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        let mut debug = f.debug_struct("GeneralRegistersDiff");

        for i in Self::INDEX_RANGE {
            if i == Self::RSP_INDEX {
                continue;
            }

            let old = self.old.get_reg_of_index(i);
            let new = self.new.get_reg_of_index(i);

            if old != new {
                debug.field(
                    GeneralRegisters::register_name(i),
                    &format!("{:#x} -> {:#x}", old, new),
                );
            }
        }

        debug.finish()
    }
}
