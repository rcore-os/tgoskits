#[derive(Default)]
#[repr(C)]
pub struct GeneralPurposeRegisters([usize; 32]);

/// Index of risc-v general purpose registers in `GeneralPurposeRegisters`.
#[allow(missing_docs)]
#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GprIndex {
    Zero = 0,
    RA,
    SP,
    GP,
    TP,
    T0,
    T1,
    T2,
    S0,
    S1,
    A0,
    A1,
    A2,
    A3,
    A4,
    A5,
    A6,
    A7,
    S2,
    S3,
    S4,
    S5,
    S6,
    S7,
    S8,
    S9,
    S10,
    S11,
    T3,
    T4,
    T5,
    T6,
}

impl GprIndex {
    /// Get register index from raw value.
    pub fn from_raw(raw: u32) -> Option<Self> {
        use GprIndex::*;
        let index = match raw {
            0 => Zero,
            1 => RA,
            2 => SP,
            3 => GP,
            4 => TP,
            5 => T0,
            6 => T1,
            7 => T2,
            8 => S0,
            9 => S1,
            10 => A0,
            11 => A1,
            12 => A2,
            13 => A3,
            14 => A4,
            15 => A5,
            16 => A6,
            17 => A7,
            18 => S2,
            19 => S3,
            20 => S4,
            21 => S5,
            22 => S6,
            23 => S7,
            24 => S8,
            25 => S9,
            26 => S10,
            27 => S11,
            28 => T3,
            29 => T4,
            30 => T5,
            31 => T6,
            _ => {
                return None;
            }
        };
        Some(index)
    }
}

impl GeneralPurposeRegisters {
    /// Returns the value of the given register.
    pub fn reg(&self, reg_index: GprIndex) -> usize {
        self.0[reg_index as usize]
    }

    /// Sets the value of the given register.
    pub fn set_reg(&mut self, reg_index: GprIndex, val: usize) {
        if reg_index == GprIndex::Zero {
            return;
        }

        self.0[reg_index as usize] = val;
    }

    /// Returns the argument registers.
    /// This is avoids many calls when an SBI handler needs all of the argmuent regs.
    pub fn a_regs(&self) -> &[usize] {
        &self.0[GprIndex::A0 as usize..=GprIndex::A7 as usize]
    }

    /// Returns the arguments register as a mutable.
    pub fn a_regs_mut(&mut self) -> &mut [usize] {
        &mut self.0[GprIndex::A0 as usize..=GprIndex::A7 as usize]
    }
}
