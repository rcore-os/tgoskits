use bit_field::BitField;
use bitflags::bitflags;

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
    pub fn from(int_type: InterruptType, vector: u8, has_error_code: bool) -> Self {
        let mut bits = vector as u32;
        bits.set_bits(8..11, int_type as u32);
        let mut info = Self::from_bits_retain(bits) | Self::VALID;
        if has_error_code {
            info |= Self::ERROR_CODE;
        }
        info
    }
}
