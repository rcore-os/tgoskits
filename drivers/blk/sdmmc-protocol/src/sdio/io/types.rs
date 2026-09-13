use core::num::NonZeroU16;

use crate::error::Error;

/// SDIO function number encoded in CMD52 and CMD53.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct FunctionNumber(u8);

impl FunctionNumber {
    /// Function zero, which owns CCCR, FBR, and CIS registers.
    pub const COMMON: Self = Self(0);

    /// Construct a function number in the SDIO-defined range `0..=7`.
    pub const fn new(number: u8) -> Result<Self, Error> {
        if number <= 7 {
            Ok(Self(number))
        } else {
            Err(Error::InvalidArgument)
        }
    }

    /// Return the numeric function identifier.
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Return whether this is an I/O function rather than function zero.
    pub const fn is_io(self) -> bool {
        self.0 != 0
    }
}

/// A 17-bit address in an SDIO function address space.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct IoAddress(u32);

impl IoAddress {
    /// Construct a checked SDIO address.
    pub const fn new(address: u32) -> Result<Self, Error> {
        if address <= 0x1_ffff {
            Ok(Self(address))
        } else {
            Err(Error::InvalidArgument)
        }
    }

    /// Return the raw 17-bit address.
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Address update behavior for CMD53 transfers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddressMode {
    /// Repeatedly access the same FIFO address.
    Fixed,
    /// Increment the address after each byte.
    Incrementing,
}

/// Wire transfer mode for CMD53.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransferMode {
    /// Transfer between one and 512 bytes using byte mode.
    Byte,
    /// Transfer one or more complete blocks using the configured block size.
    Block { block_size: NonZeroU16 },
}

/// Parsed manufacturer tuple from one SDIO CIS chain.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CisInfo {
    /// Function-zero address of the first tuple.
    pub pointer: u32,
    /// Manufacturer code from `CISTPL_MANFID`, when present.
    pub manufacturer_id: Option<u16>,
    /// Product code from `CISTPL_MANFID`, when present.
    pub product_id: Option<u16>,
}

/// Information discovered for one I/O function.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SdioFunctionInfo {
    /// Function number discovered through the card OCR and FBR table.
    pub number: FunctionNumber,
    /// Standard or extended interface code reported by the FBR.
    pub interface_code: u8,
    /// Block size currently programmed for this function.
    pub block_size: Option<NonZeroU16>,
    /// Whether the function has reached the CCCR I/O-ready state.
    pub enabled: bool,
    /// Whether the CCCR interrupt master and this function bit are enabled.
    pub interrupt_enabled: bool,
    /// Parsed function CIS information.
    pub cis: CisInfo,
}

/// Information published after a successful IO-only card initialization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SdioCardInfo {
    pub rca: u16,
    pub ocr: u32,
    pub io_functions: u8,
    pub cccr_revision: u8,
    pub sd_revision: u8,
    pub common_cis: CisInfo,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_identifiers_reject_out_of_range_values() {
        assert_eq!(FunctionNumber::new(7).unwrap().get(), 7);
        assert_eq!(FunctionNumber::new(8), Err(Error::InvalidArgument));
        assert_eq!(IoAddress::new(0x1_ffff).unwrap().get(), 0x1_ffff);
        assert_eq!(IoAddress::new(0x2_0000), Err(Error::InvalidArgument));
    }
}
