use bit_field::BitField;
use bitflags::bitflags;

use crate::msr::{Msr, MsrReadWrite};

bitflags! {
    /// VM_CR MSR flags.
    pub struct VmCrFlags: u64 {
        const DPD      = 1 << 0;
        const R_INIT   = 1 << 1;
        const DIS_A20M = 1 << 2;
        const LOCK     = 1 << 3;
        const SVMDIS   = 1 << 4;
    }
}

/// The VM_CR MSR controls global SVM behavior.
pub struct VmCr;

impl MsrReadWrite for VmCr {
    const MSR: Msr = Msr::VM_CR;
}

impl VmCr {
    pub fn read() -> VmCrFlags {
        VmCrFlags::from_bits_truncate(Self::read_raw())
    }
}

bitflags! {
    /// VMCB clean bits. A clear bit means the corresponding cached state is dirty.
    pub struct VmcbCleanBits: u32 {
        const I          = 1 << 0;
        const IOPM       = 1 << 1;
        const ASID       = 1 << 2;
        const TPR        = 1 << 3;
        const NP         = 1 << 4;
        const CR_X       = 1 << 5;
        const DR_X       = 1 << 6;
        const DT         = 1 << 7;
        const SEG        = 1 << 8;
        const CR2        = 1 << 9;
        const LBR        = 1 << 10;
        const AVIC       = 1 << 11;
        const CET        = 1 << 12;
        const UNMODIFIED = 0xffff_ffff;
    }
}

bitflags! {
    /// EXITINTINFO/EVENTINJ field flags in the VMCB.
    pub struct VmcbIntInfo: u32 {
        const ERROR_CODE = 1 << 11;
        const VALID      = 1 << 31;
    }
}

#[repr(u32)]
#[derive(Debug)]
pub enum InterruptType {
    External  = 0,
    Nmi       = 2,
    Exception = 3,
    SoftIntr  = 4,
}

impl VmcbIntInfo {
    fn has_error_code(vector: u8) -> bool {
        matches!(vector, 8 | 10 | 11 | 12 | 13 | 14 | 17)
    }

    pub fn from(int_type: InterruptType, vector: u8) -> Self {
        let mut bits = vector as u32;
        bits.set_bits(8..11, int_type as u32);
        let mut info = Self::from_bits_retain(bits) | Self::VALID;
        if Self::has_error_code(vector) {
            info |= Self::ERROR_CODE;
        }
        info
    }
}

#[repr(u8)]
#[derive(Debug)]
pub enum VmcbTlbControl {
    DoNotFlush         = 0,
    FlushAll           = 0x01,
    FlushAsid          = 0x03,
    FlushAsidNonGlobal = 0x07,
}
